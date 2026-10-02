import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	if (!process.env.TMUX || !process.env.TMUX_PANE || !process.stdin.isTTY || !process.stdout.isTTY) return;
	const helper = join(homedir(), ".local/lib/tmux-workspace/agent-hook.py");
	let current: { session_id: string; cwd: string; pid: number } | undefined;
	let timer: Timer | undefined;

	// Agent status. agent-status.conf holds the marks and the turn-over rule.
	// These are write commands, not statuses. Turn over lets tmux choose done or idle.
	type Write = "working" | "blocked" | "idle" | "over" | "unset";
	const pane = process.env.TMUX_PANE as string;
	const set: Record<Write, string[]> = {
		working: ["-p", "-t", pane, "@workspace_agent", "working"],
		blocked: ["-p", "-t", pane, "@workspace_agent", "blocked"],
		idle: ["-p", "-t", pane, "@workspace_agent", "idle"],
		over: ["-pF", "-t", pane, "@workspace_agent", "#{?#{@agent_turn_over},#{E:@agent_turn_over},idle}"],
		unset: ["-pu", "-t", pane, "@workspace_agent"],
	};
	const pending = new Set<string>();
	let plan = false;
	let failed = false;
	let retrying = false;
	let stopped = false;
	let idle = 0;
	let settle: Timer | undefined;
	// Until its first turn a session rests at plain idle, so it never inherits done.
	let rest: Write = "over";
	let wanted: Write | undefined;
	let written: Write | undefined;
	let writing = false;
	let draining: Promise<void> | undefined;

	// An unhandled rejection exits omp, so this function must never reject.
	async function drain() {
		writing = true;
		while (wanted && wanted !== written) {
			written = wanted;
			let ok = false;
			try {
				const result = await pi.exec("tmux", ["-N", "set", ...set[written]], { cwd: "/", timeout: 2000 });
				ok = result.code === 0 && !result.killed;
			} catch {}
			// A failed write is forgotten, so the timer writes it again a second later.
			if (!ok) {
				written = undefined;
				break;
			}
		}
		writing = false;
	}

	// Keeps only the latest wanted value, so a burst of events gives one write.
	function report(value: Write) {
		wanted = value;
		if (!writing) draining = drain();
	}

	// Subagents get their own copy of this extension. Only the root session reports.
	function root(ctx: ExtensionContext) {
		return !stopped && ctx.hasUI && ctx.mode === "tui" && ctx.agent?.kind === "main";
	}

	function recompute(ctx: ExtensionContext, settled = false) {
		if (!root(ctx)) return;
		let next: Write = rest;
		if (pending.size || plan || failed) next = "blocked";
		else if (!ctx.isIdle() || retrying || ctx.getAsyncJobSnapshot()?.running.length) next = "working";
		idle = next === rest ? idle + 1 : 0;
		// The timer settles to idle only after three consecutive idle readings.
		if (next !== rest || settled || idle >= 3) report(next);
	}

	function recomputeSoon(ctx: ExtensionContext) {
		if (settle) ctx.clearTimer(settle);
		settle = ctx.setTimeout(() => recompute(ctx, true), 1000);
	}

	function reset(ctx: ExtensionContext) {
		if (!root(ctx)) return;
		pending.clear();
		plan = failed = retrying = false;
		written = undefined;
		recomputeSoon(ctx);
	}

	function isPlanProposal(result: unknown) {
		const xdev = (result as { details?: { xdev?: { tool?: unknown; mode?: unknown } } } | null)?.details?.xdev;
		return xdev?.tool === "propose" && xdev.mode === "execute";
	}

	function send(event: "SessionStart" | "SessionEnd", value: NonNullable<typeof current>) {
		spawnSync("/usr/bin/python3", [helper, "omp"], {
			input: JSON.stringify({ ...value, hook_event_name: event }),
			stdio: ["pipe", "ignore", "ignore"],
			timeout: 3000,
		});
	}

	function update(ctx: ExtensionContext) {
		// Task/subagent runners have no UI, even though they share process.pid.
		if (!ctx.hasUI) return;
		const session_id = ctx.sessionManager.getSessionFile();
		const cwd = ctx.sessionManager.getCwd();
		if (!session_id) {
			if (current) send("SessionEnd", current);
			current = undefined;
			return;
		}
		if (current?.session_id === session_id && current.cwd === cwd) return;
		current = { session_id, cwd, pid: process.pid };
		send("SessionStart", current);
		// The helper unsets the status of a new session, so the next write must happen.
		written = undefined;
	}

	pi.on("session_start", (_event, ctx) => {
		if (!ctx.hasUI) return;
		update(ctx);
		stopped = false;
		rest = "idle";
		reset(ctx);
		// /move can relocate the same session without emitting a switch event.
		// Only changed path/cwd values invoke the helper; idle ticks do no I/O.
		if (!timer) timer = ctx.setInterval(() => {
			update(ctx);
			recompute(ctx);
		}, 1000);
	});
	pi.on("session_switch", (_event, ctx) => {
		update(ctx);
		reset(ctx);
	});
	pi.on("session_branch", (_event, ctx) => {
		update(ctx);
		reset(ctx);
	});
	pi.on("session_tree", (_event, ctx) => {
		update(ctx);
		reset(ctx);
	});
	pi.on("session_compact", (_event, ctx) => {
		// Compaction keeps a failed turn and a pending plan. Only the write is repeated.
		if (!root(ctx)) return;
		written = undefined;
		recompute(ctx);
	});
	pi.on("agent_start", (_event, ctx) => {
		if (!root(ctx)) return;
		pending.clear();
		plan = failed = false;
		rest = "over";
		if (settle) ctx.clearTimer(settle);
		settle = undefined;
		idle = 0;
		report("working");
	});
	pi.on("agent_end", (event, ctx) => {
		// omp continues by itself after willContinue, so the pane stays working.
		if (!root(ctx) || event.willContinue) return;
		pending.clear();
		const last = event.messages.findLast(message => message.role === "assistant");
		failed = (last as { stopReason?: string } | undefined)?.stopReason === "error";
		recomputeSoon(ctx);
	});
	pi.on("tool_approval_requested", (event, ctx) => {
		if (!root(ctx)) return;
		pending.add(event.toolCallId);
		recompute(ctx);
	});
	pi.on("tool_approval_resolved", (event, ctx) => {
		if (!root(ctx)) return;
		pending.delete(event.toolCallId);
		recompute(ctx);
	});
	pi.on("tool_execution_start", (event, ctx) => {
		if (!root(ctx) || event.toolName !== "ask") return;
		pending.add(event.toolCallId);
		recompute(ctx);
	});
	pi.on("tool_execution_end", (event, ctx) => {
		if (!root(ctx)) return;
		if (event.toolName === "ask") pending.delete(event.toolCallId);
		// A submitted plan counts as blocked until the next agent_start.
		else if (event.toolName === "write" && !event.isError && isPlanProposal(event.result)) plan = true;
		else return;
		recompute(ctx);
	});
	pi.on("auto_retry_start", (_event, ctx) => {
		retrying = true;
		recompute(ctx);
	});
	pi.on("auto_retry_end", (_event, ctx) => {
		retrying = false;
		recompute(ctx);
	});
	pi.on("session_shutdown", (_event, ctx) => {
		// Stop handling events, then unset the status after the queued writes.
		const wasRoot = root(ctx);
		stopped = true;
		if (settle) ctx.clearTimer(settle);
		settle = undefined;
		if (wasRoot) report("unset");
		if (timer) ctx.clearTimer(timer);
		timer = undefined;
		if (current) send("SessionEnd", current);
		current = undefined;
		return draining;
	});
}
