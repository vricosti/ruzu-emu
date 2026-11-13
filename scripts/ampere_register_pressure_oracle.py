"""Regenerate register-pressure fixtures from read-only Fritchii headers.
Run in a VS developer shell: python scripts/ampere_register_pressure_oracle.py <Fritchii>
"""
import pathlib
import random
import re
import subprocess
import sys
root = pathlib.Path(__file__).resolve().parents[1]
upstream = pathlib.Path(sys.argv[1]).resolve()
base = upstream / "src/shader_recompiler/frontend/ampere"
header = (base / "register_pressure.h").read_text()
instruction = (base / "instruction.h").read_text()
revision = subprocess.check_output(["git", "-C", str(upstream), "rev-parse", "HEAD"], text=True).strip()
work = root / "target/ampere-register-pressure-oracle"
work.mkdir(parents=True, exist_ok=True)
cpp = "#include <array>\n#include <vector>\n#include <cstdint>\n#include <iostream>\nusing u32=uint32_t; using u64=uint64_t;\n"
cpp += instruction[instruction.index("namespace Shader::Ampere {"):]
cpp += header[header.index("namespace Shader::Ampere {"):]
cpp += r"""
int main() {
 u32 reserved; Shader::Ampere::Instruction input{};
 while (std::cin >> reserved >> input.words[0] >> input.words[1] >> input.words[2] >> input.words[3]) {
  const auto result = Shader::Ampere::PrepareRegisterPressure(input,reserved);
  std::cout << result.valid;
  for (auto word: result.instruction.words) std::cout << ' ' << word;
  std::cout << '|';
  for (auto word: result.prefix) std::cout << std::hex << word << ' ';
  std::cout << '|';
  for (auto word: result.suffix) std::cout << std::hex << word << ' ';
  std::cout << std::dec << '\n';
 }
}
"""
(work / "oracle.cpp").write_text(cpp)
subprocess.run(["cl", "/nologo", "/std:c++20", "/EHsc", "/O2", str(work / "oracle.cpp"), "/Fe:"+str(work / "oracle.exe"), "/Fo:"+str(work / "oracle.obj")], check=True)
rng = random.Random(0x525052)
rows = []
opcodes = [int(x,16) for x in re.findall(r"case (0x[0-9a-f]+):", header)] + [0,0xfff]
for op in opcodes:
 for reserved in (0,3,4,8,212,216,244,246,248):
  for sample in range(8):
   choices = [0,1,3,4,7,8,208,211,212,215,216,219,220,228,236,243,244,245,248,252,253,254,255]
   regs = [rng.choice(choices) for _ in range(4)]
   words = [op | (7 << 12) | (regs[0] << 16) | (regs[1] << 24), regs[2], regs[3] | (rng.randrange(128) << 8) | (rng.randrange(2) << 26), 0]
   rows.append(' '.join(map(str,[reserved,*words])))
rows += [' '.join(map(str,[216,*words])) for words in ([0xffe47981,0x2004,0x0c1e8b00,0xf6200],[0xffec7981,0x2004,0x0c1e8b00,0xea200])]
output = subprocess.run([str(work / "oracle.exe")], input='\n'.join(rows)+'\n', text=True, capture_output=True, check=True).stdout.splitlines()
assert len(output)==len(rows)
p = root / 'src/shader_recompiler/src/frontend/ampere/register_pressure.fixture'
p.write_bytes((f'# Verbatim register_pressure.h + instruction.h from Fritchii {revision}\n' + '\n'.join(a+'|'+b for a,b in zip(rows,output))+'\n').encode())
print(f'{len(rows)} cases -> {p}')
