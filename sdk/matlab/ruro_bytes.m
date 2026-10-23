function out = ruro_bytes(in, type)
%RURO_BYTES Converts between ruro's base64 byte fields and arrays.
%
%   b = ruro_bytes(s) decodes the base64 string s (the data of a std/Bytes
%   or std/Image message) to a uint8 row vector.
%
%   s = ruro_bytes(b) encodes the numeric array b (its bytes, as uint8) to
%   a base64 string.
%
%   v = ruro_bytes(s, 'single') decodes s and reinterprets the bytes as
%   little-endian values of that type, e.g. the x, y, z values of a
%   std/PointCloud's points.
%
%   img = ruro_bytes(msg) with a std/Image struct returns its pixels as a
%   height x width x channels uint8 array (uint16 for mono16).
%
%   s = ruro_bytes(px, 'pixels') encodes such an array back to the base64
%   data of an image, row after row.
%
%   See also RURO_NODE.

if isstruct(in)
    out = image_pixels(in);
elseif ischar(in) || isstring(in)
    raw = decode(char(in));
    if nargin > 1
        out = typecast(raw, type);
    else
        out = raw;
    end
elseif nargin > 1 && strcmp(type, 'pixels')
    % Channels vary fastest, then columns, then rows.
    px = permute(in, [3 2 1]);
    if ~isa(px, 'uint8') && ~isa(px, 'uint16')
        px = uint8(px);
    end
    out = encode(typecast(px(:)', 'uint8'));
else
    if ~isa(in, 'uint8')
        in = typecast(in(:)', 'uint8');
    end
    out = encode(uint8(in(:)'));
end
end

function px = image_pixels(img)
raw = decode(img.data);
switch lower(img.encoding)
    case 'rgb8'
        c = 3;
    case 'rgba8'
        c = 4;
    case 'mono8'
        c = 1;
    case 'mono16'
        raw = typecast(raw, 'uint16');
        c = 1;
end
% Rows are stored one after another; MATLAB arrays are column-major.
px = permute(reshape(raw, c, img.width, img.height), [3 2 1]);
end

% matlab.net.base64decode and base64encode exist in MATLAB and in Octave 7
% and newer.
function raw = decode(s)
raw = uint8(matlab.net.base64decode(s));
raw = raw(:)';
end

function s = encode(raw)
s = char(matlab.net.base64encode(raw));
end
