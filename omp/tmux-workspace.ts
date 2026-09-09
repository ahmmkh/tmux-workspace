import { spawnSync } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	if (!process.env.TMUX || !process.env.TMUX_PANE || !process.stdin.isTTY || !process.stdout.isTTY) return;
	const helper = join(homedir(), ".local/lib/tmux-workspace/agent-hook.py");
	let current: { session_id: string; cwd: string; pid: number } | undefined;
	let timer: Timer | undefined;

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
	}

	pi.on("session_start", (_event, ctx) => {
		if (!ctx.hasUI) return;
		update(ctx);
		// /move can relocate the same session without emitting a switch event.
		// Only changed path/cwd values invoke the helper; idle ticks do no I/O.
		if (!timer) timer = ctx.setInterval(() => update(ctx), 1000);
	});
	pi.on("session_switch", (_event, ctx) => update(ctx));
	pi.on("session_branch", (_event, ctx) => update(ctx));
	pi.on("session_tree", (_event, ctx) => update(ctx));
	pi.on("session_shutdown", (_event, ctx) => {
		if (timer) ctx.clearTimer(timer);
		timer = undefined;
		if (current) send("SessionEnd", current);
		current = undefined;
	});
}
