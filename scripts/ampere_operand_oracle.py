"""Generate operand-helper fixtures from verbatim Eden-NX2 C++ lambdas.

Run inside a Visual Studio developer shell, with upstream read-only:
python scripts/ampere_operand_oracle.py ../eden-nx2
The compiler is used only to regenerate fixtures; cargo tests need no C++ toolchain.
"""
import pathlib
import os
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[1]
upstream = pathlib.Path(sys.argv[1]).resolve()
revision = "origin/eden-nx2"
source_path = "src/shader_recompiler/frontend/ampere/lower_to_maxwell.h"
source = subprocess.check_output(
    ["git", "-C", str(upstream), "show", f"{revision}:{source_path}"], text=True
)
commit = subprocess.check_output(
    ["git", "-C", str(upstream), "rev-parse", revision], text=True
).strip()
types = source[source.index("struct Instruction {"):source.index("// Lower only operations")]
uniform = source[source.index("        const auto uniform_register"):source.index("        bool rewritten_uniform")]
helpers = source[source.index("        const auto base ="):source.index("        switch (opcode)")]
rewrite = source[source.index("        bool rewritten_uniform"):source.index("        const auto base =")]
guards = source[source.index("        const u32 guard_mask"):source.index("        // Pd = LUT")]
epilogue = source[source.index("        if (!operand_error.empty())"):source.index("    return fail(static_cast<u32>(code.size())")]
# Exclude only the enclosing program loop's closing brace.
epilogue = epilogue[:epilogue.rfind("    }")]
scan = source[source.index("    bool uses_uniform_bank"):source.index("    const auto address_of")]
opcode_arms = source[source.index("        case 0x304:"):source.index("        case 0x949:")]
work = root / "target" / "ampere-operand-oracle"
work.mkdir(parents=True, exist_ok=True)
cpp = r'''
#include <array>
#include <bit>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <string_view>
#include <vector>
using u32 = uint32_t; using u64 = uint64_t; using s32 = int32_t; using s64 = int64_t;
'''+types+r'''
bool uniform_case(Instruction insn, LoweredProgram& result, std::string_view& operand_error) {
    u32 opcode = insn.Bits(0, 12), index = 0;
    const auto fail = [&](u32, std::string_view error) { operand_error = error; return false; };
'''+uniform+rewrite+r'''
    const auto emit = [&](u64 word) { result.instructions.push_back(word); };
'''+guards+r'''
    emit(opcode);
    for (auto word : insn.words) emit(word);
    emit(rewritten_uniform); emit(native_predicate); emit(guarded_uniform);
    emit(guard_mask); emit(output_mask); emit(predicate);
'''+epilogue+r'''
    return true;
}
bool scan_case(Instruction insn) {
    const std::array code{insn};
'''+scan+r'''
    return uses_uniform_bank;
}
bool opcode_case(Instruction insn, u32 index, LoweredProgram& result, std::string_view& error, const Instruction& previous, bool rewritten_uniform) {
    std::vector<Instruction> code;
    if (insn.Bits(0, 12) == 0x287 || insn.Bits(0, 12) == 0x887) {
        code.resize(index + 1);
        if (index) code[index - 1] = previous;
    }
    struct Relocation { u32 output_index; s64 target; };
    std::vector<Relocation> relocations;
    const auto fail = [&](u32, std::string_view reason) {
        result.instructions.clear(); error = reason; return false;
    };
    const u32 opcode = insn.Bits(0, 12), native_predicate = insn.Bits(12, 4);
    u64 predicate = u64{native_predicate} << 16;
    const u64 dest = insn.Bits(16, 8);
    const bool guarded_uniform = false;
    const u32 guard_slot = 6, uniform_output = 7;
'''+uniform+helpers+r'''
    switch (opcode) {
'''+opcode_arms+r'''
    default: std::abort();
    }
    if (!operand_error.empty()) return fail(index, operand_error);
    // Test-only trailer: final predicate and pending relocation list.
    emit(predicate); emit(relocations.size());
    for (auto relocation : relocations) {
        emit(relocation.output_index); emit(static_cast<u64>(relocation.target));
    }
    return true;
}
int main() {
    u32 action, p[8]; Instruction insn;
    while (std::cin >> action) {
        for (auto& value : p) std::cin >> value;
        for (auto& value : insn.words) std::cin >> value;
        const Instruction previous{{p[4], p[5], p[6], p[7]}};
        LoweredProgram result;
        u64 predicate = u64{insn.Bits(12, 4)} << 16;
        u64 dest = insn.Bits(16, 8);
        const bool guarded_uniform = false;
        const u32 guard_slot = 6, uniform_output = 7;
'''+uniform+helpers+r'''
        u32 value = 0;
        switch (action) {
        case 0: value = uniform_register(p[0]); break;
        case 1: value = valid_cbuf(); emit(cbuf()); break;
        case 2: value = materialize(p[0], p[1], p[2], p[3], p[4]); break;
        case 3: value = operand(p[0], p[1]); break;
        case 4: {
            auto half = half_source(p[0]); value = half.reg;
            extract_half(half, p[1], p[2]); break;
        }
        case 5: predicate_lut(p[0], p[1], p[2], p[3], p[4], p[5]); break;
        case 6: xmad(p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]); break;
        case 7: multiply_low(p[0], p[1], p[2], p[3]); break;
        case 8: save_predicate(p[0]); restore_predicate(p[0]); break;
        case 9: value = uniform_case(insn, result, operand_error); break;
        case 10: value = scan_case(insn); break;
        case 11: value = opcode_case(insn, p[0], result, operand_error, previous, p[2] != 0); break;
        case 12:
            result.required_texture_buffer = p[1];
            value = opcode_case(insn, p[0], result, operand_error, previous, p[2] != 0);
            result.instructions.push_back(result.required_texture_buffer);
            break;
        }
        std::cout << action;
        for (auto v : p) std::cout << ' ' << v;
        for (auto v : insn.words) std::cout << ' ' << v;
        std::cout << '|' << value << '|' << operand_error << '|';
        for (auto word : result.instructions) std::cout << std::hex << word << ' ';
        std::cout << std::dec << '\n';
    }
}
'''
(work / "oracle.cpp").write_text(cpp, encoding="utf-8")
subprocess.run(["cl", "/nologo", "/std:c++20", "/EHsc", "oracle.cpp", "/Fe:oracle.exe"], cwd=work, check=True)
cases = []


def instruction(**fields):
    bits = 7 << 12 | 19 << 16
    for name, (width, value) in fields.items():
        offset = int(name[1:])
        mask = ((1 << width) - 1) << offset
        bits = (bits & ~mask) | ((value << offset) & mask)
    return [(bits >> (32 * i)) & 0xffffffff for i in range(4)]


def case(action, params, words=None):
    cases.append(" ".join(map(str, [action, *params, *([0] * (8-len(params))), *(words or instruction())])))


for reg in range(64):
    case(0, [reg])
for bank in range(32):
    for offset in (0, 3, 65532):
        case(1, [], instruction(b54=(5, bank), b38=(16, offset)))
case(1, [], instruction(b54=(5, 24), b91=(1, 1)))
for kind in range(4):
    for reg in (0, 21, 22, 27, 28, 63, 255):
        for modifiers in range(4):
            case(2, [kind, reg, 250, modifiers & 1, modifiers >> 1], instruction(b32=(32, 0xfedcba98)))
case(2, [2, 0, 251, 1, 1], instruction(b54=(5, 24), b38=(16, 0x124)))
for form in range(8):
    for which in range(3):
        for floating in range(2):
            case(3, [which, floating], instruction(b9=(3, form), b24=(8, 3), b32=(8, 22), b64=(8, 5),
                                                  b62=(2, 3), b72=(4, 15)))
for form in range(8):
    for which in range(3):
        for swizzle in range(4):
            for lane in range(2):
                case(4, [which, lane, 20], instruction(b9=(3, form), b24=(8, 3), b32=(8, 22), b64=(8, 5),
                     b60=(2, swizzle), b62=(2, 3), b72=(2, 3), b74=(2, swizzle), b81=(2, swizzle), b83=(2, 3)))
for uniform in range(2):
    for c in range(16):
        case(5, [0x96, 1, 10, c, 4, uniform])
for immediate in range(2):
    for high_a in range(2):
        for high_b in range(2):
            case(6, [20, 3, 0xfedcba98 if immediate else 4, 5, high_a, high_b, 3, immediate])
    case(7, [3, 0xfedcba98 if immediate else 4, 5, immediate])
for mask in (0, 1, 64, 127):
    case(8, [mask])
for opcode in range(4096):
    case(10, [], instruction(b0=(12, opcode)))
case(10, [], instruction(b0=(12, 0x984), b91=(1, 1)))
case(10, [], instruction(b0=(12, 0x81c), b67=(1, 1)))
for opcode in (0x2a4, 0x8a4, 0x891, 0x890, 0x290, 0x292, 0x892, 0x291, 0xabb, 0xab9, 0xc82, 0x882, 0x202):
    for predicate in range(16):
        case(9, [], instruction(b0=(12, opcode), b12=(4, predicate), b16=(8, 21), b24=(8, 22),
                               b32=(8, 27), b64=(8, 63), b80=(4, 14), b91=(1, 1)))
for opcode in (0x292, 0x892):
    for output in range(16):
        for predicate in (0, 7, 8, 15):
            case(9, [], instruction(b0=(12, opcode), b12=(4, predicate), b80=(4, output)))
for offset in (16, 24, 32, 64):
    for register in (27, 28, 62, 63, 64, 255):
        case(9, [], instruction(b0=(12, 0x2a4), **{f'b{offset}': (8, register)}))
float_opcodes = (0x304, 0x23e, 0xa3e, 0x232, 0x832, 0xa32, 0x230, 0x430, 0x630,
                 0x231, 0x431, 0x631, 0x831, 0xa31, 0x240, 0x840)
for opcode in float_opcodes:
    words = instruction(b0=(12, opcode), b24=(8, 3), b32=(8, 4), b64=(8, 5))
    if opcode == 0x304:
        words = instruction(b0=(12, opcode), b32=(8, 4), b64=(32, 0x00200800))
    case(11, [12], words)
    # Perturb every non-opcode bit to check reserved bits, validation order,
    # all source/destination fields and scheduling bits from a valid baseline.
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for opcode in (0x231, 0x431, 0x631, 0x831, 0xa31):
    for predicate in range(16):
        for modifiers in range(8):
            case(11, [65535], instruction(b0=(12, opcode), b12=(4, predicate), b24=(8, 3), b32=(8, 4), b64=(8, 5),
                b73=(1, modifiers & 1), b74=(2, (modifiers >> 1) & 3), b79=(1, 1), b87=(4, 7)))
for swizzle in (2, 3):
    case(11, [12], instruction(b0=(12, 0x230), b24=(8, 255), b32=(8, 4), b72=(1, 1),
                               b78=(1, 1), b60=(2, swizzle)))
for immediate in (0x7c00fc00, 0x7fff8000, 0x0001ffff, 0x3c00bc00):
    case(11, [12], instruction(b0=(12, 0x840), b24=(8, 3), b32=(32, immediate)))
for opcode in (0xc24, 0xe24, 0x224, 0x2a5, 0x827, 0xa24, 0x424, 0x624, 0x824):
    words = instruction(b0=(12, opcode), b16=(8, 20), b24=(8, 3), b32=(8, 4),
                        b64=(32, 0x0f8e0006 if opcode == 0x2a5 else 0x078e00ff if opcode == 0x827 else 0x078e0005))
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for multiplier in (0, 1, 3, 48, 0xffff, 0x10001, 0xffffffff, *(1 << bit for bit in range(1, 32))):
    for addend in (5, 255):
        for negate in (0, 1):
            case(11, [12], instruction(b0=(12, 0x824), b24=(8, 3), b32=(32, multiplier),
                                       b64=(32, 0x078e0200 | addend | (negate << 11))))
for dest in (0, 1, 20, 21, 22, 26, 27, 28, 63, 255):
    for addend in (0, 1, 20, 21, 22, 26, 27, 28, 63, 255):
        case(11, [12], instruction(b0=(12, 0x2a5), b16=(8, dest), b24=(8, 3), b32=(8, 4),
                                   b64=(32, 0x0f8e0000 | addend)))
for opcode in (0x224, 0x424, 0x624):
    for negate in (0, 1):
        case(11, [12], instruction(b0=(12, opcode), b24=(8, 255), b32=(32, 255),
                                   b64=(32, 0x078e00ff | (negate << 11))))
for condition in range(16):
    case(11, [12], instruction(b0=(12, 0x624), b24=(8, 255), b32=(32, 0),
                               b64=(32, 0x000e06ff | (condition << 23))))
uniform_cases = [
    instruction(b0=(12, 0x806), b16=(8, 3), b64=(32, 0x03840100)),
    instruction(b0=(12, 0x886), b16=(8, 3), b64=(32, 0x03840100)),
    instruction(b0=(12, 0x3c2), b16=(8, 3), b24=(8, 4), b64=(32, 0x000e0000)),
    instruction(b0=(12, 0x81c), b16=(8, 0), b64=(32, 0x03f41e76)),
    instruction(b0=(12, 0x89c), b16=(8, 0), b64=(32, 0x03f41e76)),
    instruction(b0=(12, 0x883), b16=(8, 3), b24=(8, 63), b32=(32, 0x7f), b64=(32, 0x08000000)),
    instruction(b0=(12, 0x882), b16=(8, 3), b32=(32, 0xfedcba98)),
    instruction(b0=(12, 0xc82), b16=(8, 3), b32=(32, 22), b64=(32, 0x08000000)),
    instruction(b0=(12, 0xab9), b16=(8, 2), b54=(5, 24), b38=(16, 0x124), b73=(3, 4)),
    instruction(b0=(12, 0xabb), b16=(8, 2), b24=(8, 22), b73=(3, 5), b91=(1, 1)),
    instruction(b0=(12, 0x899), b16=(8, 3), b24=(8, 63), b32=(32, 12), b64=(32, 0x08011404)),
    instruction(b0=(12, 0xb82), b16=(8, 2), b24=(8, 4), b38=(16, 3), b73=(3, 5)),
    instruction(b0=(12, 0xf89), b24=(8, 4), b53=(5, 17), b40=(13, 0x1c1f), b58=(2, 2), b64=(32, 0x000e0000)),
    instruction(b0=(12, 0x822), b24=(8, 4), b32=(32, 0x93), b64=(32, 5)),
]
for words in uniform_cases:
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for lut in range(256):
    for uniform in (0, 1):
        case(11, [12], instruction(b0=(12, 0x81c), b16=(8, 0), b64=(3, lut & 7), b72=(5, lut >> 3),
                                   b67=(1, uniform), b68=(4, 7), b77=(4, 7), b81=(3, 3), b84=(3, 7), b87=(4, 9)))
for predicate in range(16):
    for target in range(8):
        case(11, [12], instruction(b0=(12, 0x89c), b12=(4, predicate), b16=(8, 0), b64=(3, 6), b72=(5, 0x12),
                                   b68=(4, predicate), b77=(4, 15-predicate), b81=(3, target), b84=(3, 7), b87=(4, 7)))
for mask in range(128):
    case(11, [12], instruction(b0=(12, 0x883), b16=(8, 3), b24=(8, 63), b32=(32, mask), b64=(32, 0x08000000)))
for opcode in (0xab9, 0xabb, 0xb82):
    for bank in range(32):
        for width in range(8):
            case(11, [12], instruction(b0=(12, opcode), b16=(8, 2), b24=(8, 3), b54=(5, bank),
                                       b38=(16, 0x124), b73=(3, width), b91=(1, opcode == 0xabb)))
for opcode in (0x886, 0x3c2, 0x883, 0x882, 0xc82, 0xab9, 0xabb):
    for target in (15, 16, 21, 22, 26, 27, 28, 63, 64, 254, 255):
        words = next(w.copy() for w in uniform_cases if w[0] & 0xfff == opcode)
        words[0] = (words[0] & ~0xff0000) | (target << 16)
        case(11, [12], words)
for shift in range(34):
    for low, high_word in ((63, 0x08011404), (4, 0x0800163f), (63, 0x08011604), (63, 0x08010604), (4, 0x0800063f)):
        case(11, [12], instruction(b0=(12, 0x899), b16=(8, 3), b24=(8, low), b32=(32, shift), b64=(32, high_word)))
for swizzle in range(256):
    case(11, [12], instruction(b0=(12, 0x822), b24=(8, 4), b32=(32, swizzle), b64=(32, 5)))
texture_cases = [
    instruction(b0=(12, 0xb6c), b16=(8, 2), b24=(8, 4), b32=(8, 6), b40=(14, 3), b54=(5, 2), b61=(3, 5), b64=(32, 0x001e0f04)),
    instruction(b0=(12, 0x989), b24=(8, 4), b53=(5, 17), b58=(2, 3), b64=(32, 0x000e0006)),
    instruction(b0=(12, 0x589), b24=(8, 4), b32=(8, 6), b40=(13, 0x1c1f), b64=(32, 0x000e0000)),
    instruction(b0=(12, 0x98e), b24=(8, 4), b32=(8, 6), b64=(32, 0x0010e100)),
    instruction(b0=(12, 0x2bd), b16=(8, 3), b32=(32, 4), b64=(32, 0x080e0000)),
    instruction(b0=(12, 0x2be), b16=(8, 3), b32=(32, 4), b64=(32, 0x08000000)),
    instruction(b0=(12, 0x3a1), b16=(8, 255), b24=(8, 4), b64=(32, 0x00060200)),
    instruction(b0=(12, 0x3c4), b16=(8, 3), b24=(8, 4), b64=(32, 0xc000)),
    instruction(b0=(12, 0xf8c), b16=(8, 255), b24=(8, 4), b64=(32, 0x0d00003f)),
    instruction(b0=(12, 0x38c), b24=(8, 4), b32=(8, 6), b40=(24, 0x104)),
    instruction(b0=(12, 0x3a8), b24=(8, 4), b32=(8, 6), b40=(24, 0x104), b64=(32, 0x001ee100)),
    instruction(b0=(12, 0x9a8), b24=(8, 255), b32=(8, 6), b40=(24, 0x104), b64=(32, 0x081ee144)),
    instruction(b0=(12, 0xb97), b24=(8, 4), b40=(14, 3), b54=(5, 2), b61=(3, 3), b64=(32, 0x001e0f00)),
    instruction(b0=(12, 0xb9b), b24=(8, 4), b32=(8, 6), b40=(14, 3), b54=(5, 2), b61=(3, 3), b64=(32, 0x00100f00)),
]
for words in texture_cases:
    case(12, [12, 0xffffffff], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(12, [12, 0xffffffff], changed)
for opcode in (0x98e, 0x3a8, 0x9a8):
    for offset in (-0x800001, -0x800000, -0x80001, -0x80000, -1, 0, 0x7ffff, 0x80000, 0x7fffff):
        for address in (0, 1, 20, 22, 26, 27, 28, 63):
            word2 = 0x0c10e100 | address if opcode == 0x98e else 0x081ee140 | address if opcode == 0x9a8 else 0x001ee100
            case(12, [12, 0xffffffff], instruction(b0=(12, opcode), b24=(8, address if opcode == 0x3a8 else 255),
                                                  b32=(8, 6), b40=(24, offset), b64=(32, word2)))
for operation in range(8):
    for uniform in (False, True):
        case(12, [12, 0xffffffff], instruction(b0=(12, 0x98e), b24=(8, 255 if uniform else 4), b32=(8, 6),
            b64=(32, 0x0c10e104 if uniform else 0x0010e100), b87=(3, operation)))
for opcode in (0xb6c, 0xb97, 0xb9b):
    words = next(w.copy() for w in texture_cases if w[0] & 0xfff == opcode)
    for current in (0xffffffff, 0, 2, 17):
        case(12, [12, current], words)
    for binding in range(32):
        changed = words.copy()
        changed[1] = (changed[1] & ~(31 << 22)) | (binding << 22)
        case(12, [12, 0xffffffff], changed)
for dimension in range(8):
    for coordinate in (252, 253, 254, 255):
        for opcode in (0xb97, 0xb9b):
            words = next(w.copy() for w in texture_cases if w[0] & 0xfff == opcode)
            words[0] = (words[0] & 0x00ffffff) | (coordinate << 24)
            words[1] = (words[1] & ~(7 << 29)) | (dimension << 29)
            case(12, [12, 0xffffffff], words)
for maximum in (False, True):
    for target in (0, 15, 16, 21, 22, 27, 28, 63):
        case(12, [12, 0xffffffff], instruction(b0=(12, 0x3c4), b16=(8, target), b24=(8, 4), b64=(32, 0x14000 if maximum else 0xc000)))
for stride in range(4):
    for address in (4, 255):
        case(12, [12, 0xffffffff], instruction(b0=(12, 0xf8c), b16=(8, 255), b24=(8, address), b64=(32, 0x0d00003f | (stride << 14))))
bits_cases = [
    instruction(b0=(12, 0x301), b32=(32, 4)),
    instruction(b0=(12, 0x219), b24=(8, 4), b32=(32, 6), b64=(32, 0x000006ff)),
    instruction(b0=(12, 0xc19), b24=(8, 4), b32=(32, 6), b64=(32, 0x080006ff)),
    instruction(b0=(12, 0x300), b32=(32, 4), b64=(32, 0x000e0000)),
    instruction(b0=(12, 0xd00), b32=(32, 4), b64=(32, 0x080e0000)),
    instruction(b0=(12, 0x309), b32=(32, 4)),
    instruction(b0=(12, 0xd09), b32=(32, 4), b64=(32, 0x08000000)),
    instruction(b0=(12, 0x918)),
    instruction(b0=(12, 0x992), b76=(3, 2)),
    instruction(b0=(12, 0x9ab)),
    instruction(b0=(12, 0xb1d)),
    instruction(b0=(12, 0x388), b24=(8, 4), b32=(8, 6), b40=(24, 0x100), b73=(3, 4), b78=(2, 2)),
    instruction(b0=(12, 0x984), b24=(8, 4), b40=(24, 0x100), b73=(3, 4), b78=(2, 2)),
    instruction(b0=(12, 0x23a), b24=(8, 4)),
]
for words in bits_cases:
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for opcode in (0x219, 0xc19):
    for low, high in ((4, 255), (255, 4), (4, 6), (255, 255)):
        for flags in range(8):
            for shift in (0, 15, 21, 22, 27, 28, 63, 64, 255, 256):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, low), b32=(32, shift),
                     b64=(32, (0x08000600 if opcode == 0xc19 else 0x600) | high),
                     b75=(1, flags & 1), b76=(1, (flags >> 1) & 1), b80=(1, flags >> 2)))
for scope in range(8):
    for predicate in (0, 7, 8, 15):
        case(11, [12], instruction(b0=(12, 0x992), b12=(4, predicate), b76=(3, scope)))
for opcode in (0x388, 0x984):
    for width in range(8):
        for stride in range(4):
            for address in (4, 255):
                for base in (0, 21, 22, 27, 28, 63):
                    case(11, [12], instruction(b0=(12, opcode), b24=(8, address), b32=(8, base), b40=(24, 0xfffff0),
                                               b73=(3, width), b78=(2, stride), b91=(1, opcode == 0x984)))
for target in (0, 245, 246, 255):
    for source_reg in (0, 245, 246, 255):
        case(11, [12], instruction(b0=(12, 0x23a), b16=(8, target), b24=(8, source_reg)))
words = instruction(b0=(12, 0x83b), b16=(8, 20), b24=(8, 4), b40=(24, 0x124), b72=(2, 2))
case(11, [12], words)
for bit in range(12, 128):
    changed = words.copy()
    changed[bit // 32] ^= 1 << (bit % 32)
    case(11, [12], changed)
for count in range(4):
    for transpose in (0, 1):
        for target in (0, 20, 242, 244, 245, 246):
            for address in (4, target, target + 1, 255):
                case(11, [12], instruction(b0=(12, 0x83b), b16=(8, target), b24=(8, address),
                                           b40=(24, 0xfffffc), b72=(2, count), b78=(1, transpose)))


def write_fixture(name, vectors, tensor_mode=None):
    env = os.environ.copy()
    env.pop('EDEN_NX2_TENSOR', None)
    if tensor_mode is not None:
        env['EDEN_NX2_TENSOR'] = tensor_mode
    output = subprocess.check_output([str(work / 'oracle.exe')], input='\n'.join(vectors)+'\n', text=True, env=env)
    fixture = root / 'src/shader_recompiler/src/frontend/ampere' / name
    fixture.write_text(f'# Generated from {commit}:{source_path}\n# scripts/ampere_operand_oracle.py; verbatim C++ blocks; tensor={tensor_mode!r}\n'+output, encoding='utf-8')
    print(f'Wrote {len(vectors)} upstream cases to {fixture}')


special_cases = [
    instruction(b0=(12, 0x919), b72=(8, 33)),
    instruction(b0=(12, 0x9c3), b16=(8, 3), b72=(8, 33)),
    instruction(b0=(12, 0x28c), b24=(8, 3), b32=(8, 4), b64=(32, 0x03f21000)),
    instruction(b0=(12, 0x88c), b24=(8, 3), b32=(32, 0xfedcba98), b64=(32, 0x03f21000)),
    instruction(b0=(12, 0x287), b16=(8, 3), b24=(8, 4), b32=(8, 5), b64=(32, 0x08800000)),
    instruction(b0=(12, 0x887), b16=(8, 3), b24=(8, 4), b32=(32, 0xfedcba98), b64=(32, 0x08800000)),
    instruction(b0=(12, 0x381), b16=(8, 20), b24=(8, 4), b40=(24, 0x1234), b64=(32, 0x001e8900)),
    instruction(b0=(12, 0x981), b16=(8, 20), b24=(8, 4), b32=(8, 6), b40=(24, 0x1234), b64=(32, 0x081e8900)),
    instruction(b0=(12, 0x386), b24=(8, 4), b32=(8, 6), b40=(24, 0x1234), b64=(32, 0x0010a900)),
    instruction(b0=(12, 0x986), b24=(8, 4), b32=(8, 6), b40=(24, 0x1234), b64=(32, 0x0810a906)),
    instruction(b0=(12, 0x95b), b64=(32, 0x03800000)),
]
for words in special_cases:
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for special in range(256):
    for opcode in (0x919, 0x9c3):
        case(11, [12], instruction(b0=(12, opcode), b16=(8, 3), b72=(8, special)))
for opcode in (0x28c, 0x88c):
    for output_predicate in range(8):
        for comparison in range(8):
            for signed in (0, 1):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, 3), b32=(32, 4), b73=(1, signed),
                    b76=(3, comparison), b81=(3, output_predicate), b84=(3, 7), b87=(4, 7)))
previous = instruction(b0=(12, 0x88c), b24=(8, 4), b32=(32, 100), b64=(32, 0x0bf01070))
current = instruction(b0=(12, 0x887), b16=(8, 3), b24=(8, 4), b32=(32, 100), b64=(32, 0x08000000))
case(11, [1, 0, 0, 0, *previous], current)
case(11, [0, 0, 0, 0, *previous], current)
for bit in range(128):
    changed = previous.copy()
    changed[bit // 32] ^= 1 << (bit % 32)
    case(11, [1, 0, 0, 0, *changed], current)
for limit in (0x7ffff, 0x80000, 0xffffffff):
    prior, now = previous.copy(), current.copy()
    prior[1] = now[1] = limit
    case(11, [1, 0, 0, 0, *prior], now)
for opcode in (0x381, 0x981, 0x386, 0x986):
    for size in range(8):
        for data in (0, 1, 2, 4, 243, 244, 245, 246, 255):
            for address in (4, 5, 244, 246, 252, 254, 255):
                store = opcode in (0x386, 0x986)
                word2 = {0x381: 0x001e8100, 0x981: 0x081e8100, 0x386: 0x0010a100, 0x986: 0x0810a106}[opcode]
                case(11, [12], instruction(b0=(12, opcode), b16=(8, 20 if store else data), b24=(8, address),
                    b32=(8, data if store else 6 if opcode == 0x981 else 0), b40=(24, 0xffffff),
                    b64=(32, word2), b73=(3, size), b90=(1, opcode in (0x981, 0x986))))
fetch_cases = [
    instruction(b0=(12, 0x305), b32=(32, 4), b64=(32, 0x0021f000)),
    instruction(b0=(12, 0xb05), b38=(16, 0x124), b54=(5, 2), b64=(32, 0x0021f000)),
    instruction(b0=(12, 0xd05), b32=(32, 4), b64=(32, 0x0821f000)),
    instruction(b0=(12, 0xb6f), b16=(8, 20), b24=(8, 4), b40=(14, 3), b54=(5, 2), b64=(32, 0x3ff)),
    instruction(b0=(12, 0xb66), b16=(8, 20), b24=(8, 4), b32=(8, 6), b40=(14, 3), b54=(5, 2), b60=(1, 1), b61=(3, 1), b64=(32, 0x008e03ff)),
    instruction(b0=(12, 0x91a), b16=(8, 0), b32=(32, 0xd040)),
]
for words in fetch_cases:
    case(12, [12, 0xffffffff], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(12, [12, 0xffffffff], changed)
for opcode in (0x305, 0xb05, 0xd05):
    for mode in (0x0010f000, 0x00212800, 0x00207000, 0x00203000, 0x0021f000):
        for modifiers in (0, 0x100, 0x10000, 0x10100):
            for reg in (0, 21, 22, 27, 28, 63, 255):
                case(12, [12, 0xffffffff], instruction(b0=(12, opcode), b32=(32, reg),
                    b64=(32, mode | modifiers | (0x08000000 if opcode == 0xd05 else 0))))
for dimension in range(8):
    for mask in range(16):
        for second in (22, 23, 254, 255):
            case(12, [12, 0xffffffff], instruction(b0=(12, 0xb66), b16=(8, 20), b24=(8, 4), b32=(8, 6),
                b40=(14, 3), b54=(5, 2), b60=(1, 1), b61=(3, dimension), b64=(8, second), b72=(4, mask), b81=(3, 7), b87=(3, 1)))
for opcode in (0xb6f, 0xb66):
    baseline = next(w.copy() for w in fetch_cases if w[0] & 0xfff == opcode)
    for current_binding in (0xffffffff, 0, 2, 17):
        case(12, [12, current_binding], baseline)
    for dest in (1, 20, 252, 253, 254, 255):
        changed = baseline.copy()
        changed[0] = (changed[0] & ~0xff0000) | (dest << 16)
        case(12, [12, 0xffffffff], changed)
sample_cases = [
    instruction(b0=(12, 0xb69), b16=(8, 20), b24=(8, 4), b32=(8, 255), b40=(14, 3), b54=(5, 2), b61=(3, 1), b64=(32, 0x3ff)),
    instruction(b0=(12, 0xb63), b16=(8, 20), b24=(8, 4), b32=(8, 6), b40=(14, 3), b54=(5, 2), b61=(3, 1), b64=(32, 0x000e0f16)),
    instruction(b0=(12, 0xb60), b16=(8, 20), b24=(8, 4), b32=(8, 6), b40=(14, 3), b54=(5, 2), b60=(1, 1), b61=(3, 1), b64=(32, 0x000e03ff)),
]
for words in sample_cases:
    case(12, [12, 0xffffffff], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(12, [12, 0xffffffff], changed)
for dimension in range(8):
    for lod in range(8):
        for depth in (0, 1):
            for offsets in (0, 1):
                for half in (0, 1):
                    for second in (22, 255):
                        for mask in (1, 3, 15):
                            case(12, [12, 0xffffffff], instruction(b0=(12, 0xb60), b16=(8, 20), b24=(8, 4), b32=(8, 8),
                                b40=(14, 3), b54=(5, 2), b60=(1, 1), b61=(3, dimension), b64=(8, second), b72=(4, mask),
                                b76=(1, offsets), b78=(1, depth), b79=(1, half), b81=(3, 7), b87=(3, lod)))
for opcode in (0xb69, 0xb63, 0xb60):
    for mask in range(16):
        for dest in (20, 254, 255):
            for second in (22, 254, 255):
                words = next(w.copy() for w in sample_cases if w[0] & 0xfff == opcode)
                words[0] = (words[0] & ~0xff0000) | (dest << 16)
                words[2] = (words[2] & ~0xfff) | (mask << 8) | second
                case(12, [12, 0xffffffff], words)
for binding in (0, 2, 17, 18):
    for initial in (0xffffffff, 0, 2, 17):
        for dimension, depth, offsets in ((1, 0, 0), (5, 1, 0), (7, 0, 1)):
            for source_a in (244, 245, 246, 248, 249, 255):
                case(12, [12, initial], instruction(b0=(12, 0xb60), b16=(8, 20), b24=(8, source_a), b32=(8, 4), b40=(14, 3),
                    b54=(5, binding), b60=(1, 1), b61=(3, dimension), b64=(8, 255), b72=(4, 1), b76=(1, offsets), b78=(1, depth), b81=(3, 7)))
move_cases = [
    instruction(b0=(12, 0x816), b24=(8, 4), b32=(32, 0x5410), b64=(8, 8)),
    instruction(b0=(12, 0x816), b24=(8, 4), b32=(32, 0xf280), b64=(8, 8)),
    instruction(b0=(12, 0x307), b32=(8, 4), b75=(2, 2), b84=(2, 2)),
    instruction(b0=(12, 0x321), b24=(8, 255), b32=(8, 4), b40=(10, 0x100)),
    instruction(b0=(12, 0x322), b24=(8, 255), b32=(8, 4), b40=(10, 0x100), b64=(8, 8)),
    instruction(b0=(12, 0x387), b16=(8, 0), b24=(8, 255), b32=(8, 4), b40=(24, 16), b64=(32, 0x00100800)),
    instruction(b0=(12, 0x983), b24=(8, 255), b40=(24, 16), b64=(32, 0x00300800)),
    instruction(b0=(12, 0x803), b24=(8, 4), b32=(32, 0x7f)),
    instruction(b0=(12, 0x804), b16=(8, 0), b24=(8, 4), b32=(32, 0x7f)),
    instruction(b0=(12, 0xc02), b32=(32, 15), b64=(32, 0x08000f00)),
    instruction(b0=(12, 0x202), b32=(8, 4), b72=(4, 15)),
    instruction(b0=(12, 0x802), b32=(32, 0xdeadbeef), b72=(4, 15)),
    instruction(b0=(12, 0xa02), b40=(14, 4), b54=(5, 2), b72=(4, 15)),
]
for words in move_cases:
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
# Exercise each selector in every byte, both sources, sign replication, and fast paths.
for selector in list(range(16)) + [0x5410, 0x5432, 0x7610, 0x7632, 0xffff, 0x10000]:
    for lane in range(4):
        for a in (4, 249, 250, 251, 254, 255):
            for c in (8, 250, 255):
                case(11, [12], instruction(b0=(12, 0x816), b24=(8, a),
                    b32=(32, selector << (lane * 4) if selector < 16 else selector), b64=(8, c)))
for rounding in range(4):
    for modifiers in range(8):
        for pred in (0, 7, 8, 15):
            case(11, [12], instruction(b0=(12, 0x307), b12=(4, pred), b32=(8, 255),
                b62=(2, modifiers & 3), b75=(2, 2), b78=(2, rounding), b80=(1, modifiers >> 2), b84=(2, 2)))
for opcode in (0x321, 0x322):
    for address in (0, 1, 0x5c, 0x3f0, 0x3f4, 0x3f8, 0x3fc, 0x3ff):
        for components in range(4):
            for reg in (19, 249, 250, 251, 252, 253, 254, 255):
                case(11, [12], instruction(b0=(12, opcode), b16=(8, reg), b24=(8, 255),
                    b32=(8, reg), b40=(10, address), b64=(8, 8), b74=(2, components)))
for opcode in (0x387, 0x983):
    for offset in (0, 1, 3, 4, 0x7ffffc, 0x7fffff, 0x800000, 0xfffffc):
        for reg in (0, 19, 247, 248, 255):
            case(11, [12], instruction(b0=(12, opcode), b16=(8, 0 if opcode == 0x387 else reg),
                b24=(8, 255), b32=(8, reg if opcode == 0x387 else 0), b40=(24, offset),
                b64=(32, 0x00100800 if opcode == 0x387 else 0x00300800)))
for opcode in (0x803, 0x804):
    for mask in (0, 1, 0x3f, 0x40, 0x7f, 0x80, 0xffffffff):
        for byte in range(4):
            case(11, [12], instruction(b0=(12, opcode), b16=(8, 0), b24=(8, 255), b32=(32, mask), b76=(2, byte)))
for reg in (0, 15, 16, 21, 22, 27, 28, 63, 255):
    case(11, [12], instruction(b0=(12, 0xc02), b32=(32, reg), b64=(32, 0x08000f00)))
for bank in (0, 17, 18, 23, 24, 25, 31):
    for offset in (0, 1, 3, 4, 0x3ffc, 0x3fff):
        case(11, [12], instruction(b0=(12, 0xa02), b40=(14, offset), b54=(5, bank), b72=(4, 15)))
float_arithmetic = (0x223, 0x423, 0x623, 0x823, 0xa23, 0xc23, 0xe23,
    0x220, 0x820, 0xa20, 0xc20, 0x221, 0x421, 0x621, 0x821, 0xa21, 0xe21,
    0x209, 0x809, 0xa09, 0xc09)
float_cases = []
for opcode in float_arithmetic:
    words = instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8), b64=(8, 12),
        b84=(3, 4 if opcode & 0x1ff == 0x20 else 0),
        b91=(1, int(opcode in (0xc23, 0xe23, 0xc20, 0xe21, 0xc09))))
    float_cases.append(words)
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
    # Operand selection, cbuf/uniform bounds, and immediate bit patterns.
    for field in (24, 32, 64):
        for reg in (0, 15, 21, 22, 27, 28, 63, 247, 250, 255):
            changed = words.copy()
            changed[field // 32] = (changed[field // 32] & ~(0xff << (field % 32))) | (reg << (field % 32))
            case(11, [12], changed)
    for operand_word in (0, 0x80000000, 0x3f800000, 0x7fc00000, 0xffffffff, 0x06000400, 0x04800400):
        changed = words.copy()
        changed[1] = operand_word
        case(11, [12], changed)
for opcode in (0x220, 0x820, 0xa20, 0xc20):
    for scale in range(8):
        for rounding in range(4):
            for ftz in (0, 1):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8),
                    b78=(2, rounding), b80=(1, ftz), b84=(3, scale), b91=(1, int(opcode == 0xc20))))
for opcode in (0x209, 0x809, 0xa09, 0xc09):
    for pred in range(16):
        for select in range(16):
            case(11, [12], instruction(b0=(12, opcode), b12=(4, pred), b24=(8, 4), b32=(8, 8),
                b87=(4, select), b91=(1, int(opcode == 0xc09))))
special_cases = [
    instruction(b0=(12, 0x308), b32=(8, 4)),
    instruction(b0=(12, 0x308), b32=(8, 4), b73=(1, 1)),
    instruction(b0=(12, 0xb08), b40=(14, 4), b54=(5, 2)),
    instruction(b0=(12, 0x526), b24=(8, 4), b32=(8, 255), b64=(8, 12), b81=(3, 7)),
    instruction(b0=(12, 0x326), b32=(8, 255), b64=(8, 12), b81=(3, 7)),
    instruction(b0=(12, 0x81a), b24=(8, 4), b32=(32, 16)),
    instruction(b0=(12, 0x299), b24=(8, 4), b32=(8, 8), b64=(32, 0x0800063f)),
    instruction(b0=(12, 0x419), b24=(8, 255), b32=(32, 0xdeadbeef), b64=(32, 0x00010e04)),
    instruction(b0=(12, 0x805), b64=(32, 0x0001ff00)),
    instruction(b0=(12, 0x306), b32=(8, 4), b64=(32, 0x1000)),
    instruction(b0=(12, 0x306), b32=(8, 4), b64=(32, 0x00101000)),
    instruction(b0=(12, 0xd06), b32=(8, 4), b64=(32, 0x08001000)),
    instruction(b0=(12, 0x245), b32=(8, 4), b64=(32, 0x00201000)),
    instruction(b0=(12, 0xa45), b40=(14, 4), b54=(5, 2), b64=(32, 0x00201000)),
    instruction(b0=(12, 0xc45), b32=(8, 4), b64=(32, 0x08201000)),
]
for words in special_cases:
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
for opcode in (0x308, 0xb08):
    for operation in range(64):
        for half in (0, 1):
            for lane in range(4):
                case(11, [12], instruction(b0=(12, opcode), b32=(8, 4), b60=(2, lane), b73=(1, half), b74=(6, operation)))
for opcode in (0x526, 0x326):
    for index_register in (0, 21, 22, 27, 28, 63, 255):
        for mode in range(16):
            for attribute in (0, 63, 255):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, index_register), b32=(8, 255),
                    b64=(8, attribute), b76=(4, mode), b81=(3, 7)))
for count in (0, 1, 8, 16, 31, 32, 33, 0xffffffff):
    for dest in (0, 19, 254, 255):
        case(11, [12], instruction(b0=(12, 0x81a), b16=(8, dest), b24=(8, 255), b32=(32, count)))
for high_result in (0, 1):
    for low in (4, 28, 63):
        for high in (8, 28, 63):
            for right in (0, 1):
                for wrap in (0, 1):
                    for shift in (0, 21, 22, 27, 28, 63):
                        case(11, [12], instruction(b0=(12, 0x299), b24=(8, low), b32=(8, shift), b64=(8, high),
                            b73=(2, 3), b75=(1, wrap), b76=(1, right), b80=(1, high_result), b91=(1, 1)))
for dest in (0, 19, 247, 248, 249, 250, 254, 255):
    case(11, [12], instruction(b0=(12, 0x805), b16=(8, dest), b64=(32, 0x1ff00)))
for opcode in (0x306, 0xd06, 0x245, 0xa45, 0xc45):
    for reg in (0, 21, 22, 27, 28, 63, 255):
        for lane in range(4):
            for mode in (0x1000, 0x00101000, 0x08001000, 0x00201000, 0x08201000):
                case(11, [12], instruction(b0=(12, opcode), b32=(8, reg), b60=(2, lane), b64=(32, mode)))
for opcode in (0x245, 0xa45, 0xc45):
    for rounding in range(4):
        for signed in (0, 1):
            case(11, [12], instruction(b0=(12, opcode), b32=(8, 8), b64=(32, 0x08201000 if opcode == 0xc45 else 0x00201000),
                b74=(1, signed), b78=(2, rounding)))
logic_opcodes = (0x207, 0x807, 0xa07, 0x208, 0x808, 0xa08, 0x20a, 0x80a, 0xa0a, 0x434,
    0x20b, 0x80b, 0xa0b, 0x20c, 0x80c, 0xa0c, 0xc0c, 0x212, 0x812, 0xa12, 0xc12,
    0x213, 0xa13, 0xc13, 0x217, 0x817, 0xa17, 0xc17, 0x819)
for opcode in logic_opcodes:
    low = opcode & 0x1ff
    mode = 0x078ee408 if low == 0x12 else 0x03800000 if low == 0x17 else 0x00010608 if opcode == 0x819 else 0
    if opcode in (0xc13, 0xc17): mode |= 0x08000000
    words = instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8), b64=(32, mode))
    case(11, [12], words)
    for bit in range(12, 128):
        changed = words.copy()
        changed[bit // 32] ^= 1 << (bit % 32)
        case(11, [12], changed)
    for value in (0, 21, 22, 27, 28, 63, 255, 256, 0x7ffff, 0x80000, 0xffffffff, 0x06000400):
        changed = words.copy()
        changed[1] = value
        case(11, [12], changed)
for opcode in (0x20a, 0x80a, 0xa0a, 0x20b, 0x80b, 0xa0b, 0x20c, 0x80c, 0xa0c, 0xc0c):
    for compare in range(16):
        for combine in range(4):
            for pred in (0, 7, 8, 15):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8),
                    b74=(2, combine), b76=(4, compare), b87=(4, pred)))
for opcode in (0x212, 0x812, 0xa12, 0xc12):
    for lut in range(256):
        for target in (0, 6, 7):
            case(11, [12], instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8),
                b64=(8, 12), b72=(8, lut), b81=(3, target), b87=(4, 15)))
for swizzle in range(4):
    for combine in range(4):
        for compare in range(16):
            case(11, [12], instruction(b0=(12, 0x434), b24=(8, 4), b32=(32, 0x3c00bc00),
                b69=(2, combine), b71=(1, 1), b74=(2, swizzle), b76=(4, compare), b81=(3, 2), b84=(3, 3), b87=(4, 15)))
for high_result in (0, 1):
    for right in (0, 1):
        for mode in range(4):
            for wrap in (0, 1):
                for low, high in ((4, 8), (255, 8), (4, 255), (255, 255)):
                    for shift in (0, 1, 31, 32, 33, 0xffffffff):
                        case(11, [12], instruction(b0=(12, 0x819), b24=(8, low), b32=(32, shift), b64=(8, high),
                            b73=(2, mode), b75=(1, wrap), b76=(1, right), b80=(1, high_result)))
address_add_cases = []
for opcode in (0xa11, 0x211, 0x811):
    for high in (0, 1):
        address_add_cases.append(instruction(b0=(12, opcode), b24=(8, 4), b32=(8, 8), b64=(8, 255),
            b75=(5, 3), b80=(1, high), b81=(3, 0 if opcode == 0xa11 else 7)))
for opcode in (0x210, 0x810, 0xa10, 0xc10):
    for mode in (0x07ffe0ff, 0x07f1e0ff, 0x07ffe4ff, 0x07ffe008, 0x07ffe808):
        address_add_cases.append(instruction(b0=(12, opcode), b24=(8, 255 if mode == 0x07ffe4ff else 4),
            b32=(8, 8), b64=(32, mode)))
for words in address_add_cases:
    for rewritten in (0, 1):
        case(11, [12, 0, rewritten], words)
        for bit in range(12, 128):
            changed = words.copy()
            changed[bit // 32] ^= 1 << (bit % 32)
            case(11, [12, 0, rewritten], changed)
for opcode in (0xa11, 0x211, 0x811):
    for high in (0, 1):
        for shift in (0, 1, 16, 31):
            for sign in (0, 1):
                for word in (0, 8, 255, 0x80000008, 0xffffffff, 0x06000400):
                    case(11, [12], instruction(b0=(12, opcode), b24=(8, 4), b32=(32, word), b64=(8, 255),
                        b73=(1, sign), b75=(5, shift), b80=(1, high), b81=(3, 0 if opcode == 0xa11 else 7)))
for opcode in (0x210, 0x810):
    for rewritten in (0, 1):
        for carry_in in (0, 1):
            for carry_out in range(8):
                for condition in range(16):
                    for word in (0, 8, 255, 256, 0xffffffff):
                        case(11, [12, 0, rewritten], instruction(b0=(12, opcode), b24=(8, 4), b32=(32, word),
                            b64=(8, 255), b74=(1, carry_in), b77=(4, 15), b81=(3, carry_out), b84=(3, 7), b87=(4, condition)))
for opcode in (0x210, 0x810, 0xa10, 0xc10):
    for third in (0, 8, 250, 255):
        for negations in range(8):
            for word in (0, 21, 22, 27, 28, 63, 64, 255, 256, 0x06000400):
                case(11, [12], instruction(b0=(12, opcode), b24=(8, 4), b32=(32, word),
                    b63=(1, negations & 1), b64=(8, third), b72=(1, (negations >> 1) & 1),
                    b75=(1, (negations >> 2) & 1), b77=(7, 0x7f), b84=(7, 0x7f)))
for carry_out in range(8):
    for condition in range(16):
        for carry_in in (0, 1):
            case(11, [12], instruction(b0=(12, 0xa10), b24=(8, 255), b40=(14, 4), b54=(5, 24),
                b64=(8, 255), b74=(1, carry_in), b77=(4, 15), b81=(3, carry_out), b84=(3, 7), b87=(4, condition)))
write_fixture('operand_helpers.fixture', cases)
cases = []
words = instruction(b0=(12, 0x23c), b16=(8, 20), b24=(8, 4), b32=(32, 8), b64=(32, 0x80c))
case(11, [12], words)
for bit in range(12, 128):
    changed = words.copy()
    changed[bit // 32] ^= 1 << (bit % 32)
    case(11, [12], changed)
for field in (16, 24, 32, 64):
    for reg in (20, 241, 242, 243, 244, 245, 246, 255):
        changed = words.copy()
        word, bit = divmod(field, 32)
        changed[word] = (changed[word] & ~(255 << bit)) | (reg << bit)
        case(11, [12], changed)
for predicate in range(16):
    case(11, [12], instruction(b0=(12, 0x23c), b12=(4, predicate), b16=(8, 20), b24=(8, 4), b32=(32, 8), b64=(32, 0x8ff)))
for mode in (None, 'stub', 'emulate', ''):
    suffix = 'missing' if mode is None else mode or 'empty'
    write_fixture(f'tensor_{suffix}.fixture', cases, mode)
