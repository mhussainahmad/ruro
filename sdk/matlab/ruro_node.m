function ruro_node(step, varargin)
%RURO_NODE Runs a ruro node implemented in MATLAB or GNU Octave.
%
%   ruro_node(@my_step) serves the node instance the host started this
%   program for, until the host stops it. On every step it calls
%
%       [out, state] = my_step(in, params, ctx, state)
%
%   in      struct with one field per input, each a struct array of the
%           messages queued since the last step (fields seq, stamp_ns and
%           msg), or [] when none arrived.
%   params  struct of the node's parameters, kept up to date when they are
%           changed while the graph runs.
%   ctx     struct with steps, dt and time (seconds), now_ns and estopped.
%   state   whatever my_step returned last time ([] at first).
%   out     struct with one field per output to send on: a cell array of
%           messages (a single message that is not a cell is sent as one).
%
%   Options, as name-value pairs:
%   'start'  @(params) -> state: called once before the first step; its
%            result is the first state.
%   'estop'  @(state) -> state: called when the emergency stop is engaged.
%   'stop'   @(state): called when the host stops the node.
%
%   Messages are the JSON forms of ruro's message types, decoded with
%   jsondecode: a std/Scalar is a number, a std/Pose a struct with position
%   and orientation, a std/Image a struct with width, height, encoding and
%   data (base64; see ruro_bytes). An error raised by my_step is reported to
%   the host as the step's error; the node keeps running.
%
%   MATLAB cannot read its standard input when started by another program, so
%   give MATLAB nodes "transport: tcp" in their manifest: the host then passes
%   an address in RURO_NODE_ADDR, which this function connects to. Octave can
%   use either transport.
%
%   See also RURO_BYTES.

opts = struct('start', [], 'estop', [], 'stop', []);
for i = 1:2:numel(varargin)
    opts.(varargin{i}) = varargin{i + 1};
end

io = open_channel();
hello = struct('ruro', 1);
token = getenv('RURO_NODE_TOKEN');
if ~isempty(getenv('RURO_NODE_ADDR')) && ~isempty(token)
    hello.token = token;
end
send(io, hello);

params = struct();
state = [];
while true
    req = receive(io);
    if isempty(req)
        return;  % the host went away
    end
    reply = struct();
    try
        switch req.op
            case 'start'
                params = req.params;
                if ~isempty(opts.start)
                    state = opts.start(params);
                end
            case 'step'
                ctx = rmfield(req, {'op', 'inputs'});
                [out, state] = step(req.inputs, params, ctx, state);
                reply.outputs = as_lists(out);
            case 'param'
                params.(req.name) = req.value;
            case 'estop'
                if ~isempty(opts.estop)
                    state = opts.estop(state);
                end
            case 'stop'
                if ~isempty(opts.stop)
                    opts.stop(state);
                end
                send(io, reply);
                return;
        end
    catch err
        reply.error = err.message;
    end
    send(io, reply);
end
end

function out = as_lists(out)
% Every output's messages as a list, whatever my_step returned.
if isempty(out)
    out = struct();
    return;
end
names = fieldnames(out);
for i = 1:numel(names)
    v = out.(names{i});
    if ~iscell(v)
        out.(names{i}) = {v};
    end
end
end

function io = open_channel()
addr = getenv('RURO_NODE_ADDR');
io = struct('tcp', []);
if isempty(addr)
    return;
end
colon = find(addr == ':', 1, 'last');
host = addr(1:colon - 1);
port = str2double(addr(colon + 1:end));
if exist('OCTAVE_VERSION', 'builtin')
    pkg load instrument-control
end
io.tcp = tcpclient(host, port);
end

function send(io, msg)
line = ['@ruro ', jsonencode(msg), sprintf('\n')];
if isempty(io.tcp)
    fprintf(1, '%s', line);
    if exist('OCTAVE_VERSION', 'builtin')
        fflush(stdout);
    end
else
    write(io.tcp, uint8(line));
end
end

function msg = receive(io)
% The next protocol line from the host, decoded; [] at the end.
persistent buffer
if isempty(buffer)
    buffer = uint8([]);
end
msg = [];
while true
    if isempty(io.tcp)
        line = fgetl(0);
        if ~ischar(line)
            return;
        end
    else
        nl = find(buffer == 10, 1);
        while isempty(nl)
            n = io.tcp.NumBytesAvailable;
            if n == 0
                n = 1;  % wait for at least one byte
            end
            chunk = read(io.tcp, n, 'uint8');
            if isempty(chunk)
                return;
            end
            buffer = [buffer, uint8(chunk(:)')]; %#ok<AGROW>
            nl = find(buffer == 10, 1);
        end
        line = char(buffer(1:nl - 1));
        buffer = buffer(nl + 1:end);
    end
    if strncmp(line, '@ruro ', 6)
        msg = jsondecode(line(7:end));
        return;
    end
end
end
