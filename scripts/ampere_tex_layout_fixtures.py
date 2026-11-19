"""Print a reviewed patch for frozen C++ TEX operand-layout expectations.

Does not run the Rust lowerer or write fixtures. Updates only source-register
fields in the old MOV sequence, asserting the old sequence first. The layout
comes from Mesa 61a2e907 nak_nir_lower_tex.c (non-scalar source0/source1), not
the implementation under test. Use --patch to emit apply_patch input.
"""
import pathlib
import sys

root = pathlib.Path(__file__).resolve().parents[1]
base = root / 'src/shader_recompiler/src/frontend/ampere'
changes = {}
for name in ('programs.fixture', 'operand_helpers.fixture', 'texture_sample_programs.fixture'):
    rows = []
    for line in (base / name).read_text().splitlines():
        if not line or line.startswith('#'):
            continue
        cols = line.split('|')
        if cols[2]:
            continue
        inp = list(map(int, cols[0].split()))
        if name == 'operand_helpers.fixture':
            if inp[0] not in (11, 12):
                continue
            native = [inp[-4:]]
            starts = [0]
        else:
            native = [inp[i:i + 4] for i in range(1, len(inp), 4)]
            starts = list(map(int, cols[4].split()))
        output = list(map(lambda s: int(s, 16), cols[3].split()))
        old_output = output.copy()
        for n, words in enumerate(native):
            v = sum(w << (32 * i) for i, w in enumerate(words))
            field = lambda low, size: (v >> low) & ((1 << size) - 1)
            opcode = field(0, 12)
            if opcode not in (0xb60, 0xf60) or field(60, 1):
                continue
            dim, lod, depth = field(61, 3), field(87, 3), field(78, 1)
            if dim == 5 and depth and lod == 0 and field(64, 8) == 255 and field(72, 4) == 1:
                continue  # Separately lowered array comparison, already correct.
            coords = (1, 2, 3, 3, 2, 3, 0, 4)[dim]
            extra = int(lod in (2, 3, 6, 7)) + field(76, 1) + depth
            count = coords + extra
            old_split = (count + 1) // 2 if count <= 4 else 4
            if old_split == coords:
                continue
            a, b = field(24, 8), field(32, 8)
            def source(i, split):
                reg, delta = (a, i) if i < split else (b, i - split)
                return 255 if reg == 255 else reg + delta
            start = starts[n]
            end = starts[n + 1] if n + 1 < len(starts) else len(output)
            # The coordinate MOVs precede the metadata/texture operation.
            matches = [j for j in range(start, end)
                       if output[j] >> 48 == 0x5c98 and output[j] & 255 == 250]
            assert len(matches) == 1, (name, cols[0], matches)
            begin = matches[0]
            indices = list(range(begin, begin + coords))
            cursor = begin + coords
            # Uniform handle MOV or second-CBUF handle LDC precedes metadata.
            if opcode == 0xf60 or (output[cursor] >> 48 == 0x4c98):
                cursor += 1
            indices.extend(range(cursor, cursor + extra))
            for i, j in enumerate(indices):
                assert output[j] >> 48 == 0x5c98, (name, hex(output[j]))
                assert (output[j] >> 20) & 255 == source(i, old_split), (name, cols[0], i)
                output[j] = (output[j] & ~(255 << 20)) | (source(i, coords) << 20)
        if output != old_output:
            cols[3] = ' '.join(f'{w:x}' for w in output) + ' '
            rows.append((line, '|'.join(cols).rstrip()))
    changes[name] = rows

if '--patch' in sys.argv:
    print('*** Begin Patch')
    for name, rows in changes.items():
        if not rows:
            continue
        print('*** Update File: src/shader_recompiler/src/frontend/ampere/' + name)
        for old, new in rows:
            print('@@\n-' + old + '\n+' + new)
    print('*** End Patch')
else:
    for name, rows in changes.items():
        print(name, len(rows), 'changed cases')
