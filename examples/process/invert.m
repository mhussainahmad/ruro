function [out, state] = invert(in, params, ctx, state)
% A ruro node in MATLAB or Octave: inverts 8-bit images.
out = struct();
out.inverted = {};
for i = 1:numel(in.image)
    img = in.image(i).msg;
    px = ruro_bytes(img);              % height x width x channels
    img.data = ruro_bytes(255 - px, 'pixels');
    out.inverted{end + 1} = img;
end
end
