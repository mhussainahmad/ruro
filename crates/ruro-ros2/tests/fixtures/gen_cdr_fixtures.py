"""Generates CDR fixtures with ROS 2's own serializer.

Run inside ROS 2 Jazzy with RMW_IMPLEMENTATION=rmw_zenoh_cpp, after building
interfaces/ruro_test_msgs with colcon and sourcing its install/setup.bash:

    python3 gen_cdr_fixtures.py ../data/cdr_fixtures.jsonl hashes.txt $(cat cdr_fixture_types.txt)

hashes.txt receives every type hash rosidl generated in AMENT_PREFIX_PATH;
../data/jazzy_fixture_hashes.txt is the subset for the definitions in this directory.
Each output line is {"type": ..., "cdr": hex, "value": tree}, where the tree is the
message as ROS deserializes it, in ruro's JSON convention (bytes and chars as integers,
arrays as lists, nested messages as objects in field order).
"""
import importlib
import json
import glob
import os
import random
import re
import sys

from rclpy.serialization import serialize_message, deserialize_message
from rosidl_parser import definition as d

BASIC_RANGES = {
    'int8': (-128, 127), 'uint8': (0, 255), 'int16': (-32768, 32767), 'uint16': (0, 65535),
    'int32': (-2**31, 2**31 - 1), 'uint32': (0, 2**32 - 1),
    'int64': (-2**63, 2**63 - 1), 'uint64': (0, 2**64 - 1),
}
STRINGS = ['', 'hello', 'é€ ünïcode', 'a b', 'x' * 13]
WSTRINGS = ['', 'wide', 'é€', 'emoji 😀 ok']


def msg_class(name):
    pkg, ns, ty = name.split('/')
    mod = importlib.import_module(f'{pkg}.{ns}')
    if hasattr(mod, ty):
        return getattr(mod, ty)
    base = ty.split('_')[0]
    snake = re.sub(r'(?<!^)(?=[A-Z])', '_', base).lower()
    return getattr(importlib.import_module(f'{pkg}.{ns}._{snake}'), ty)


def basic(rng, typename):
    if typename == 'boolean':
        return rng.random() < 0.5
    if typename == 'octet':
        return bytes([rng.randrange(256)])
    if typename == 'char':
        return rng.choice(['A', 'z', '\x7f', '0'])
    if typename in ('float', 'double'):
        return rng.randrange(-4000, 4000) / 8.0
    lo, hi = BASIC_RANGES[typename]
    return rng.choice([lo, hi, 0, 1, rng.randrange(lo, hi)])


def value_for(rng, t, depth):
    if isinstance(t, d.BasicType):
        return basic(rng, t.typename)
    if isinstance(t, d.BoundedString):
        return rng.choice(['', 'ab', 'xyz'])[: t.maximum_size]
    if isinstance(t, d.UnboundedString):
        return rng.choice(STRINGS)
    if isinstance(t, d.BoundedWString):
        return rng.choice(['', 'w', 'é'])[: t.maximum_size]
    if isinstance(t, d.UnboundedWString):
        return rng.choice(WSTRINGS)
    if isinstance(t, d.NamespacedType):
        return populate(rng, msg_class('/'.join(t.namespaces + [t.name]))(), depth + 1)
    if isinstance(t, d.Array):
        n = t.size
    elif isinstance(t, d.BoundedSequence):
        n = rng.randrange(0, min(t.maximum_size, 3) + 1)
    elif isinstance(t, d.UnboundedSequence):
        n = rng.randrange(0, 4) if depth < 3 else rng.randrange(0, 2)
    else:
        raise TypeError(t)
    return [value_for(rng, t.value_type, depth) for _ in range(n)]


def populate(rng, msg, depth=0):
    for (name, _), t in zip(msg.get_fields_and_field_types().items(), msg.SLOT_TYPES):
        v = value_for(rng, t, depth)
        try:
            setattr(msg, name, v)
        except Exception as e:  # noqa
            raise RuntimeError(f'{type(msg).__name__}.{name} = {v!r}: {e}')
    return msg


def tree(v, t):
    if isinstance(t, d.NamespacedType):
        return {name: tree(getattr(v, name), st)
                for (name, _), st in zip(v.get_fields_and_field_types().items(), v.SLOT_TYPES)}
    if isinstance(t, (d.Array, d.BoundedSequence, d.UnboundedSequence)):
        if isinstance(v, (bytes, bytearray)):
            return list(v)
        return [tree(x, t.value_type) for x in v]
    if isinstance(t, d.BasicType):
        if t.typename == 'octet':
            return v[0] if isinstance(v, (bytes, bytearray)) else int(v)
        if t.typename == 'char':
            return ord(v) if isinstance(v, str) else int(v)
        if t.typename == 'boolean':
            return bool(v)
        if t.typename in ('float', 'double'):
            return float(v)
        return int(v)
    return str(v)


def main():
    out, hashes_out, types = sys.argv[1], sys.argv[2], sys.argv[3:]
    with open(out, 'w') as f:
        for name in types:
            cls = msg_class(name)
            for seed in range(3):
                rng = random.Random(f'{name}/{seed}')
                msg = populate(rng, cls()) if seed else cls()
                data = serialize_message(msg)
                back = deserialize_message(data, cls)
                top = d.NamespacedType(name.split('/')[:2], name.split('/')[2])
                f.write(json.dumps({'type': name, 'cdr': data.hex(),
                                    'value': tree(back, top)}, ensure_ascii=False) + '\n')
    hashes = {}
    for prefix in os.environ['AMENT_PREFIX_PATH'].split(':'):
        for path in glob.glob(f'{prefix}/share/*/*/*.json'):
            try:
                doc = json.load(open(path))
            except Exception:
                continue
            for h in doc.get('type_hashes', []):
                hashes[h['type_name']] = h['hash_string']
    with open(hashes_out, 'w') as f:
        for k in sorted(hashes):
            f.write(f'{k} {hashes[k]}\n')


main()
