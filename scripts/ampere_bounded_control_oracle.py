"""Compile verbatim Fritchii bounded-control preflight (run in VS developer shell)."""
from pathlib import Path
import subprocess
import sys

upstream = Path(sys.argv[1])
source = (upstream / 'src/shader_recompiler/frontend/ampere/lower_to_maxwell.h').read_text()
instruction = (upstream / 'src/shader_recompiler/frontend/ampere/instruction.h').read_text().split('namespace Shader::Ampere {', 1)[1].rsplit('} // namespace Shader::Ampere', 1)[0]
a = source.index('    std::vector<bool> control_entries')
b = source.index('    for (u32 index = 0; index < code.size(); ++index) {\n        auto insn = code[index];', a)
preflight = source[a:b]
work = Path('target/ampere-bounded-control-oracle')
work.mkdir(parents=True, exist_ok=True)
cpp = '#include <array>\n#include <vector>\n#include <optional>\n#include <span>\n#include <iostream>\n#include <cstdint>\nusing u32=std::uint32_t; using u64=std::uint64_t; using s64=std::int64_t;\n'
cpp += instruction + '\nvoid analyze(std::span<const Instruction> code, bool geometry_stage, bool uses_uniform_bank, u32 high_uniform_base) {\n' + preflight
cpp += r"""
std::cout << bounded_control;
for (const auto& values : {bounded_links, direct_targets}) {
 std::cout << '|'; for (auto value : values) std::cout << value << ' ';
}
for (const auto& values : {checked_returns, dead_instructions}) {
 std::cout << '|'; for (auto value : values) std::cout << value << ' ';
}
std::cout << '\n';
}
int main() { u32 geometry, uniforms, base, size;
while (std::cin >> geometry >> uniforms >> base >> size) {
 std::vector<Instruction> code(size);
 for (auto& instruction : code) for (auto& word : instruction.words) std::cin >> word;
 analyze(code, geometry, uniforms, base);
}}
"""
(work / 'oracle.cpp').write_text(cpp)
subprocess.run(['cl', '/nologo', '/std:c++20', '/EHsc', '/O2', 'oracle.cpp', '/Fe:oracle.exe'], cwd=work, check=True)

def relative(op, pc, target, register=0, mode=0):
    value = ((target - pc - 1) * 16) & ((1 << 50) - 1)
    return [op | 0x7000 | register << 24, (value & 0xffffffff) | mode,
            (0x03c00000 if op in (0x944, 0x950) else 0x03800000) | (value >> 32), 0]

exit = [0x794d, 0, 0x03800000, 0]
nop = [0x7918, 0, 0, 0]
cases = []
for link in [0, 20, 55, 212, 216, 244, 245, 246, 249, 255]:
    mov = lambda address: [0x7802 | link << 16, address, 0xf00, 0]
    copy = [0x000a7202, link, 0xf00, 0]
    programs = [
        [mov(32), relative(0x944, 1, 3), exit, relative(0x950, 3, 0, link)],
        [mov(48), nop, relative(0x944, 2, 4), exit, relative(0x950, 4, 0, link)],
        [mov(32), relative(0x944, 1, 3), exit, copy, relative(0x950, 4, 0, 10)],
        [relative(0x947, 0, 3), nop, relative(0x950, 2, 0, link), mov(80), relative(0x944, 4, 1), exit],
        [relative(0x947, 0, 4), mov(64), nop, relative(0x944, 3, 5), exit, relative(0x950, 5, 0, link)],
    ]
    for code in programs:
        for geometry, uniform, base in [(0,0,216),(0,1,216),(0,1,212),(1,0,216)]:
            cases.append([geometry, uniform, base, len(code)] + [w for i in code for w in i])
        if link == 55:
            for index in range(len(code)):
                for bit in range(128):
                    changed = [i.copy() for i in code]
                    changed[index][bit // 32] ^= 1 << (bit % 32)
                    cases.append([0,0,216,len(code)] + [w for i in changed for w in i])
inputs = [' '.join(map(str, c)) for c in cases]
out = subprocess.check_output([str(work / 'oracle.exe')], input='\n'.join(inputs)+'\n', text=True).splitlines()
assert len(out) == len(inputs)
fixture = Path('src/shader_recompiler/src/frontend/ampere/bounded_control.fixture')
fixture.write_bytes(('# Verbatim Fritchii bounded-control preflight, including invalid partial state\n'+'\n'.join(i+'|'+o for i,o in zip(inputs,out))+'\n').encode())
print('cases', len(inputs), 'bounded', sum(o.startswith('1|') for o in out))
