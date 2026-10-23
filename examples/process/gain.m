function [out, state] = gain(in, params, ctx, state)
% A ruro node in MATLAB or Octave: multiplies every input by the parameter
% k. Runs under ruro_node; see gain-octave.node.yaml and gain-matlab.node.yaml.
out = struct();
if ~isempty(in.input)
    out.output = num2cell([in.input.msg] * params.k);
end
if ctx.estopped
    out.output = {};  % nothing moves while the e-stop is engaged
end
end
