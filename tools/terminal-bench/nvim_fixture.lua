local events = assert(os.getenv('AURA_BENCH_NVIM_EVENTS'))
local title = assert(os.getenv('AURA_BENCH_NVIM_TITLE'))
local uv = vim.uv or vim.loop
vim.o.termguicolors = true
vim.o.title = true
vim.o.titlestring = title
vim.o.laststatus = 2
vim.o.statusline = ' '
vim.o.showmode = false
vim.o.ruler = false
vim.o.showcmd = false
vim.o.cmdheight = 1
vim.o.wrap = false
vim.o.swapfile = false
local ready, count, last = false, 0, ''
local function record(value)
  value.pid = uv.os_getpid()
  value.columns = vim.o.columns
  value.rows = vim.o.lines
  value.mode = vim.api.nvim_get_mode().mode
  local file = assert(io.open(events, 'a'))
  file:write(vim.fn.json_encode(value), '\n')
  file:flush()
  file:close()
end
local function paint(rgb)
  local color = string.format('#%02x%02x%02x', rgb[1], rgb[2], rgb[3])
  vim.api.nvim_set_hl(0, 'StatusLine', {fg = '#ffffff', bg = color})
  vim.api.nvim_set_hl(0, 'StatusLineNC', {fg = '#ffffff', bg = color})
  vim.cmd('redrawstatus')
end
vim.api.nvim_create_autocmd('TextChangedI', {callback = function()
  if not ready then return end
  local text = vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]
  if text == last then return end
  count, last = count + 1, text
  local rgb = {29 + count % 101, 67 + (count * 37) % 151, 31 + (count * 53) % 191}
  paint(rgb)
  record({op = 'edit', index = count, text = text, rgb = rgb})
end})
vim.api.nvim_create_autocmd('CursorMoved', {callback = function()
  if ready then
    record({op = 'cursor', row = vim.api.nvim_win_get_cursor(0)[1],
            top = vim.fn.line('w0'), text = vim.api.nvim_get_current_line()})
  end
end})
vim.api.nvim_create_autocmd('InsertEnter', {once = true, callback = function()
  vim.schedule(function()
    -- A discovery handshake independent of terminfo's title capabilities.
    io.stdout:write('\27]0;' .. title .. '\7')
    io.stdout:flush()
    paint({41, 83, 167})
    ready = true
    record({op = 'ready', text = vim.api.nvim_buf_get_lines(0, 0, 1, false)[1]})
  end)
end})
vim.api.nvim_create_autocmd('VimEnter', {once = true, callback = function()
  vim.api.nvim_win_set_cursor(0, {1, 0})
  vim.cmd('startinsert')
end})
