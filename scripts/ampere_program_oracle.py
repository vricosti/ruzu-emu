"""Generate complete-program fixtures from the verbatim Eden-NX2 header.

Run in a Visual Studio developer shell: python scripts/ampere_program_oracle.py ../eden-nx2
Upstream is read-only. The generated fixture needs no C++ compiler at test time.
"""
import os
import pathlib
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[1]
upstream = pathlib.Path(sys.argv[1]).resolve()
revision = 'origin/eden-nx2'
source = subprocess.check_output(['git', '-C', str(upstream), 'show',
    revision + ':src/shader_recompiler/frontend/ampere/lower_to_maxwell.h'], text=True)
commit = subprocess.check_output(['git', '-C', str(upstream), 'rev-parse', revision], text=True).strip()
work = root / 'target' / 'ampere-program-oracle'
work.mkdir(parents=True, exist_ok=True)
cpp = '''#include <cstdlib>
#include <algorithm>
#include <array>
#include <bit>
#include <span>
#include <optional>
#include <string_view>
#include <vector>
#include <cstdint>
#include <iostream>
using u32 = std::uint32_t; using u64 = std::uint64_t;
using s32 = std::int32_t; using s64 = std::int64_t;
''' + source[source.index('namespace Shader::Ampere {'):] + '''
int main() {
    size_t size;
    while (std::cin >> size) {
        std::vector<Shader::Ampere::Instruction> code(size);
        for (auto& insn : code) for (auto& word : insn.words) std::cin >> word;
        auto result = Shader::Ampere::LowerToMaxwell(code);
        std::cout << result.source_size << ' ' << result.required_texture_buffer << ' ' << result.error_offset
                  << '|' << result.error << '|';
        for (auto word : result.instructions) std::cout << std::hex << word << ' ';
        std::cout << std::dec << '|';
        for (auto offset : result.source_instruction_offsets) std::cout << offset << ' ';
        std::cout << '\\n';
    }
}
'''
(work / 'oracle.cpp').write_text(cpp, encoding='utf-8')
subprocess.run(['cl', '/nologo', '/std:c++20', '/EHsc', '/O2', str(work / 'oracle.cpp'),
    '/Fe:' + str(work / 'oracle.exe'), '/Fo:' + str(work / 'oracle.obj')], check=True)

def instruction(**fields):
    value = 7 << 12
    for name, (width, field) in fields.items():
        bit = int(name[1:])
        value = (value & ~(((1 << width) - 1) << bit)) | ((field & ((1 << width) - 1)) << bit)
    return [(value >> bit) & 0xffffffff for bit in range(0, 128, 32)]

def relative(opcode, index, target, **fields):
    raw = ((target - index - 1) * 16) & ((1 << 50) - 1)
    return instruction(b0=(12, opcode), b32=(32, raw), b64=(18, raw >> 32), **fields)

exit_insn = instruction(b0=(12, 0x94d), b87=(4, 7))
nop = instruction(b0=(12, 0x918))
cases = [[], [nop], [exit_insn], [exit_insn, instruction(b0=(12, 0xfff))]]
def add(code):
    cases.append(code)

# Every previously compared instruction arm, now under full scratch/rewriting/finish checks.
seen = set()
fixture = root / 'src/shader_recompiler/src/frontend/ampere/operand_helpers.fixture'
for line in fixture.read_text().splitlines():
    if not line or line.startswith('#'): continue
    values = list(map(int, line.split('|')[0].split()))
    if values[0] not in (11, 12): continue
    words = tuple(values[-4:])
    if words in seen: continue
    seen.add(words)
    add([list(words), exit_insn])
# All opcodes, never-executed padding, global uniform-bank scan, and register boundaries.
for opcode in range(4096):
    # Tensor mode is verified in isolated processes by the dedicated tensor fixtures.
    if opcode != 0x23c:
        add([instruction(b0=(12, opcode)), exit_insn])
    add([instruction(b0=(12, opcode), b12=(4, 15)), exit_insn])
for field in (16, 24, 32, 64):
    for reg in (215, 216, 237, 238, 245, 246, 249, 254, 255):
        words = instruction(b0=(12, 0x918), **{'b' + str(field): (8, reg)})
        add([words, exit_insn])
        add([words, exit_insn, instruction(b0=(12, 0xc02), b12=(4, 15))])
# Direct branches: negative, forward, backward, unaligned and out-of-capture targets.
for index in (0, 1, 2, 3):
    for target in (-1, 0, 1, 2, 3, 4, 8):
        for pg in (0, 7, 8, 15):
            for pc in (0, 7, 8, 15):
                add([nop] * index + [relative(0x947, index, target, b12=(4, pg), b87=(4, pc)), exit_insn])
for pg in range(16):
    for pc in range(16):
        for special in range(4):
            add([instruction(b0=(12, 0x94d), b12=(4, pg), b84=(2, special), b87=(4, pc)), exit_insn])
# Valid CALL main EXIT and RET, return-link copies, and table branches.
link = 55
call = relative(0x944, 1, 3, b86=(1, 1), b87=(4, 7))
ret = relative(0x950, 3, 0, b24=(8, link), b86=(1, 1), b87=(4, 7))
baseline = [instruction(b0=(12, 0x802), b16=(8, link), b32=(32, 32), b72=(4, 15)), call, exit_insn, ret]
programs = [baseline]
for copy_op in (0x202, 0x224):
    copy = instruction(b0=(12, copy_op), b16=(8, 10), b32=(32, link if copy_op == 0x202 else 255),
        b24=(8, 0 if copy_op == 0x202 else 255), b64=(32, 0xf00 if copy_op == 0x202 else 0x078e0000 | link))
    programs.append(baseline[:3] + [copy, relative(0x950, 4, 0, b24=(8, 10), b86=(1, 1), b87=(4, 7))])
programs.append(baseline[:3] + [exit_insn])
programs.append(baseline[:3] + [nop])
for opcode in (0x949, 0x958):
    programs.append([relative(opcode, 0, 0, b24=(8, 4), b87=(4, 7), b91=(1, opcode == 0x958)),
        exit_insn, relative(0x947, 2, 2, b87=(4, 7))])
programs.append([relative(0x944, 0, 2, b12=(4, 0), b86=(1, 1), b87=(4, 7)),
    relative(0x947, 1, 0, b87=(4, 7)), exit_insn])
for program in programs:
    add(program)
    for index in range(len(program)):
        for bit in range(128):
            changed = [words.copy() for words in program]
            changed[index][bit // 32] ^= 1 << (bit % 32)
            add(changed)
for opcode in (0x945, 0x941, 0x942, 0x948):
    add([instruction(b0=(12, opcode)), exit_insn])

# Uniform rewrite + guard + output merge + carry paths together with the outer loop.
for opcode in (0x290, 0x890, 0x291, 0x891, 0x292, 0x892, 0x2a4, 0x8a4):
    for pg in (0, 1, 7, 8, 9, 15):
        for output in (0, 1, 6, 7):
            for source in (0, 21, 22, 27, 28, 63):
                mode = 0x07ffe03f if opcode in (0x290, 0x890) else 0x078e003f
                words = instruction(b0=(12, opcode), b12=(4, pg), b16=(8, 0), b24=(8, source), b32=(8, 1),
                    b64=(32, mode), b81=(3, output))
                add([words, exit_insn])
                if opcode in (0x290, 0x890):
                    words = words.copy()
                    words[2] |= 1 << 10
                    add([words, exit_insn])
# Multiple callees and linkage consistency, plus overwritten/copy link scans.
for second_link in (55, 56):
    multi = [baseline[0], relative(0x944, 1, 5, b86=(1, 1), b87=(4, 7)),
        instruction(b0=(12, 0x802), b16=(8, second_link), b32=(32, 64), b72=(4, 15)),
        relative(0x944, 3, 6, b86=(1, 1), b87=(4, 7)), exit_insn,
        relative(0x950, 5, 0, b24=(8, 55), b86=(1, 1), b87=(4, 7)),
        relative(0x950, 6, 0, b24=(8, second_link), b86=(1, 1), b87=(4, 7))]
    add(multi)
    for target in (0, 4, 5, 6, 7):
        changed = [w.copy() for w in multi]
        changed[3] = relative(0x944, 3, target, b86=(1, 1), b87=(4, 7))
        add(changed)
for middle in (nop, instruction(b0=(12, 0x202), b16=(8, 10), b32=(8, 9), b72=(4, 15)),
        instruction(b0=(12, 0x202), b16=(8, 55), b32=(8, 9), b72=(4, 15))):
    copy = instruction(b0=(12, 0x202), b16=(8, 10), b32=(8, 55), b72=(4, 15))
    add(baseline[:3] + [copy, middle, relative(0x950, 5, 0, b24=(8, 10), b86=(1, 1), b87=(4, 7))])

inputs = [' '.join(map(str, [len(code)] + [word for insn in code for word in insn])) for code in cases]
env = os.environ.copy()
env.pop('EDEN_NX2_TENSOR', None)
output = subprocess.check_output([str(work / 'oracle.exe')], input='\n'.join(inputs) + '\n', text=True, env=env)
lines = output.splitlines()
assert len(lines) == len(inputs)
destination = fixture.with_name('programs.fixture')
destination.write_text('# Verbatim complete LowerToMaxwell: ' + commit + '\n' +
    '\n'.join(i + '|' + o for i, o in zip(inputs, lines)) + '\n', encoding='utf-8')
print(f'Wrote {len(inputs)} complete programs to {destination}')
