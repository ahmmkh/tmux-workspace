local M = {}
local uv = vim.uv or vim.loop
local state
local configured = false

local function command(argv)
  local text = vim.fn.system(argv)
  if vim.v.shell_error ~= 0 then return nil, vim.trim(text) end
  return vim.trim(text)
end

local function tmux(args)
  local argv = { 'tmux', '-S', state.tmux_socket }
  vim.list_extend(argv, args)
  return command(argv)
end

local function owns(metadata)
  return type(metadata) == 'table' and metadata.kind == 'nvim'
    and metadata.pid == state.pid and metadata.birth == state.birth and metadata.id == state.path
end

local function current()
  local raw = tmux { 'show-options', '-p', '-v', '-t', state.pane, '@workspace_app' }
  if not raw or raw == '' then return '', nil end
  local ok, metadata = pcall(vim.json.decode, raw)
  return raw, ok and metadata or nil
end

-- tmux evaluates the comparison and mutation together, avoiding an exit-hook race.
local function compare_update(raw, replacement)
  local escaped = raw:gsub('#', '##'):gsub(',', '#,'):gsub('}', '#}')
  local action = 'set-option -p' .. (replacement and '' or 'u') .. ' -t '
    .. vim.fn.shellescape(state.pane) .. ' @workspace_app'
  if replacement then action = action .. ' ' .. vim.fn.shellescape(replacement) end
  return tmux { 'if-shell', '-F', '-t', state.pane, '#{==:#{@workspace_app},' .. escaped .. '}', action }
end

local function publish()
  local raw, metadata = current()
  -- A child application can temporarily own the pane while this editor remains alive.
  if raw ~= '' and not owns(metadata) then return end
  local encoded = vim.json.encode {
    kind = 'nvim', id = state.path, pid = state.pid, birth = state.birth,
    cwd = vim.fn.getcwd(), socket = state.socket,
  }
  if encoded ~= raw then compare_update(raw, encoded) end
end

function M.save()
  if not state then return nil, 'workspace recovery is inactive for this editor' end
  if state.saving then return nil, 'workspace save is already running' end
  if vim.g.SessionLoad then return nil, 'a session is still loading' end
  state.saving = true
  local options, previous_session = vim.o.sessionoptions, vim.v.this_session
  local temporary = state.path .. '.tmp'
  local ok, err = pcall(function()
    local fd, open_err = uv.fs_open(temporary, 'w', 384)
    assert(fd, open_err)
    assert(uv.fs_close(fd))
    -- Native sessions store file names/views, never unsaved contents or terminal commands.
    -- Do not serialize options/mappings or change swapfile, undofile, or user buffers.
    vim.o.sessionoptions = 'buffers,curdir,folds,tabpages,winsize'
    vim.cmd('silent noautocmd mksession! ' .. vim.fn.fnameescape(temporary))
    assert(uv.fs_chmod(temporary, 384))
    assert(uv.fs_rename(temporary, state.path))
  end)
  vim.o.sessionoptions = options
  vim.v.this_session = previous_session
  state.saving = false
  if not ok then
    uv.fs_unlink(temporary)
    return nil, tostring(err)
  end
  publish()
  return state.path
end

local function ai_ancestor(pid, pane_pid)
  local seen = {}
  while pid and pid > 1 and pid ~= pane_pid and not seen[pid] do
    seen[pid] = true
    local row = command { 'ps', '-p', tostring(pid), '-o', 'ppid=', '-o', 'command=' }
    if not row then return true end -- Do not claim a pane whose ancestry cannot be established.
    local parent, argv = row:match('^%s*(%d+)%s+(.+)$')
    if not parent then return true end
    local executable = (argv:match('^(%S+)') or ''):match('([^/]+)$')
    if executable == 'claude' or executable == 'codex' or executable == 'omp'
      or argv:find('/claude%-code/') or argv:find('/pi%-coding%-agent/')
      or argv:find('/@openai/codex/') or argv:match('^omp%s') then
      return true
    end
    pid = tonumber(parent)
  end
  return pid ~= pane_pid
end

local function start()
  if state or #vim.api.nvim_list_uis() == 0 then return end
  local tmux_socket = vim.env.TMUX and vim.env.TMUX:match('^(.*),%d+,%d+$')
  if not tmux_socket or not vim.env.TMUX_PANE then return end
  local pane_process = command {
    'tmux', '-S', tmux_socket, 'display-message', '-p', '-t', vim.env.TMUX_PANE, '#{pane_pid}',
  }
  local pane_pid = tonumber(pane_process)
  local pid = vim.fn.getpid()
  if not pane_pid or (pid ~= pane_pid and ai_ancestor(uv.os_getppid(), pane_pid)) then return end
  local birth = command { 'ps', '-p', tostring(pid), '-o', 'lstart=' }
  if not birth or birth == '' then return end
  local root = vim.fn.fnamemodify(vim.env.TMUX_WORKSPACE_STATE_DIR or (vim.fn.expand '~' .. '/.local/state/tmux-workspace'), ':p'):gsub('/$', '')
  local ok, err = pcall(function()
    vim.fn.mkdir(root, 'p', 448)
    local stat = assert(uv.fs_lstat(root))
    assert(stat.type == 'directory' and stat.uid == uv.getuid(), 'workspace directory must be owned by this user and not a symlink')
    assert(uv.fs_chmod(root, 448))
    local directory = assert(uv.fs_mkdtemp(root .. '/nvim-XXXXXX'))
    assert(uv.fs_chmod(directory, 448))
    local socket
    for _, address in ipairs(vim.fn.serverlist()) do
      local socket_stat = address:sub(1, 1) == '/' and uv.fs_lstat(address)
      if socket_stat and socket_stat.type == 'socket' and socket_stat.uid == uv.getuid() then
        assert(uv.fs_chmod(address, 384))
        socket = address
        break
      end
    end
    if not socket then
      socket = vim.fn.serverstart(directory .. '/rpc.sock')
      assert(socket ~= '', 'could not start private RPC listener')
      assert(uv.fs_chmod(socket, 384))
    end
    state = { path = directory .. '/session.vim', socket = socket, pid = pid, birth = birth,
      tmux_socket = tmux_socket, pane = vim.env.TMUX_PANE }
  end)
  if not ok then
    vim.notify('tmux workspace: ' .. tostring(err), vim.log.levels.WARN)
    return
  end
  local function save_quietly()
    local path, save_err = M.save()
    if not path and not state.warned then
      state.warned = true
      vim.notify('tmux workspace: ' .. save_err, vim.log.levels.WARN)
    elseif path then
      state.warned = false
    end
  end
  local group = vim.api.nvim_create_augroup('TmuxWorkspaceActive', { clear = true })
  local pending = false
  vim.api.nvim_create_autocmd({ 'BufWinEnter', 'TabEnter', 'WinClosed', 'DirChanged', 'SessionLoadPost' }, {
    group = group,
    callback = function()
      if pending then return end
      pending = true
      vim.defer_fn(function()
        pending = false
        if not state.leaving then save_quietly() end
      end, 500)
    end,
  })
  state.timer = uv.new_timer()
  state.timer:start(15000, 15000, vim.schedule_wrap(function()
    if not state.leaving then save_quietly() end
  end))
  vim.api.nvim_create_autocmd('VimLeavePre', {
    group = group,
    callback = function()
      state.leaving = true
      state.timer:stop()
      state.timer:close()
      M.save()
      local raw, metadata = current()
      if owns(metadata) then compare_update(raw, nil) end
    end,
  })
  save_quietly()
end

function M.setup()
  if configured or not vim.env.TMUX or not vim.env.TMUX_PANE then return end
  configured = true
  for _, arg in ipairs(vim.v.argv) do
    if arg == '--headless' or arg == '-es' or arg == '-Es' then return end
  end
  vim.api.nvim_create_autocmd({ 'VimEnter', 'UIEnter' }, {
    group = vim.api.nvim_create_augroup('TmuxWorkspaceStartup', { clear = true }),
    callback = function() vim.schedule(start) end,
  })
  if vim.v.vim_did_enter == 1 then vim.schedule(start) end
end

return M
