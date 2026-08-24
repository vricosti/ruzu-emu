// SPDX-License-Identifier: GPL-3.0-or-later
// Differential-test oracle built against Eden's Dynarmic library.

#include <array>
#include <cstdint>
#include <iomanip>
#include <iostream>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include "dynarmic/interface/A32/a32.h"

namespace {

class OracleEnvironment final : public Dynarmic::A32::UserCallbacks {
public:
    explicit OracleEnvironment(std::vector<std::uint32_t> code_)
            {
        LoadCode(0, code_);
    }

    OracleEnvironment() = default;

    void LoadCode(std::uint32_t address, const std::vector<std::uint32_t>& instructions) {
        for (std::size_t index = 0; index < instructions.size(); ++index) {
            code[address + static_cast<std::uint32_t>(index * sizeof(std::uint32_t))] =
                instructions[index];
        }
    }

    std::optional<std::uint32_t> MemoryReadCode(std::uint32_t vaddr) override {
        if (const auto iter = code.find(vaddr); iter != code.end()) {
            return iter->second;
        }
        return 0xEAFFFFFE; // b .
    }

    std::uint8_t MemoryRead8(std::uint32_t vaddr) override {
        if (const auto iter = data.find(vaddr); iter != data.end()) {
            return iter->second;
        }
        return 0;
    }

    std::uint16_t MemoryRead16(std::uint32_t vaddr) override {
        return static_cast<std::uint16_t>(MemoryRead8(vaddr)) |
               static_cast<std::uint16_t>(MemoryRead8(vaddr + 1)) << 8;
    }

    std::uint32_t MemoryRead32(std::uint32_t vaddr) override {
        return static_cast<std::uint32_t>(MemoryRead16(vaddr)) |
               static_cast<std::uint32_t>(MemoryRead16(vaddr + 2)) << 16;
    }

    std::uint64_t MemoryRead64(std::uint32_t vaddr) override {
        return static_cast<std::uint64_t>(MemoryRead32(vaddr)) |
               static_cast<std::uint64_t>(MemoryRead32(vaddr + 4)) << 32;
    }

    void MemoryWrite8(std::uint32_t vaddr, std::uint8_t value) override {
        data[vaddr] = value;
    }

    void MemoryWrite16(std::uint32_t vaddr, std::uint16_t value) override {
        MemoryWrite8(vaddr, static_cast<std::uint8_t>(value));
        MemoryWrite8(vaddr + 1, static_cast<std::uint8_t>(value >> 8));
    }

    void MemoryWrite32(std::uint32_t vaddr, std::uint32_t value) override {
        MemoryWrite16(vaddr, static_cast<std::uint16_t>(value));
        MemoryWrite16(vaddr + 2, static_cast<std::uint16_t>(value >> 16));
    }

    void MemoryWrite64(std::uint32_t vaddr, std::uint64_t value) override {
        MemoryWrite32(vaddr, static_cast<std::uint32_t>(value));
        MemoryWrite32(vaddr + 4, static_cast<std::uint32_t>(value >> 32));
    }

    bool MemoryWriteExclusive8(std::uint32_t, std::uint8_t, std::uint8_t) override {
        return true;
    }
    bool MemoryWriteExclusive16(std::uint32_t, std::uint16_t, std::uint16_t) override {
        return true;
    }
    bool MemoryWriteExclusive32(std::uint32_t, std::uint32_t, std::uint32_t) override {
        return true;
    }
    bool MemoryWriteExclusive64(std::uint32_t, std::uint64_t, std::uint64_t) override {
        return true;
    }

    void CallSVC(std::uint32_t) override {}
    void ExceptionRaised(std::uint32_t, Dynarmic::A32::Exception) override {}

    void AddTicks(std::uint64_t ticks) override {
        ticks_left = ticks >= ticks_left ? 0 : ticks_left - ticks;
    }

    std::uint64_t GetTicksRemaining() override {
        return ticks_left;
    }

private:
    std::unordered_map<std::uint32_t, std::uint32_t> code;
    std::unordered_map<std::uint32_t, std::uint8_t> data;
    std::uint64_t ticks_left = 200;
};

} // namespace

void PrintState(const Dynarmic::A32::Jit& jit) {
    std::cout << std::hex << std::setfill('0');
    for (const auto reg : jit.Regs()) {
        std::cout << std::setw(8) << reg << ' ';
    }
    std::cout << std::setw(8) << jit.Cpsr() << '\n' << std::flush;
}

int RunPersistentOracle() {
    std::uint32_t cpsr;
    std::array<std::uint32_t, 15> input_regs{};
    if (!(std::cin >> std::hex >> cpsr)) {
        return 1;
    }
    for (auto& reg : input_regs) {
        if (!(std::cin >> std::hex >> reg)) {
            return 1;
        }
    }

    OracleEnvironment environment;
    Dynarmic::A32::UserConfig config{};
    config.callbacks = &environment;
    config.optimizations = Dynarmic::no_optimizations;
    config.enable_cycle_counting = false;
    Dynarmic::A32::Jit jit{config};
    for (std::size_t index = 0; index < input_regs.size(); ++index) {
        jit.Regs()[index] = input_regs[index];
    }
    jit.SetCpsr(cpsr);
    std::cout << "OK\n" << std::flush;

    std::string command;
    while (std::cin >> command) {
        if (command == "CODE") {
            std::uint32_t address;
            std::size_t count;
            std::cin >> std::hex >> address >> count;
            std::vector<std::uint32_t> instructions(count);
            for (auto& instruction : instructions) {
                std::cin >> std::hex >> instruction;
            }
            environment.LoadCode(address, instructions);
            std::cout << "OK\n" << std::flush;
        } else if (command == "MEMW") {
            std::uint32_t address;
            std::size_t count;
            std::cin >> std::hex >> address >> count;
            for (std::size_t index = 0; index < count; ++index) {
                unsigned int byte;
                std::cin >> std::hex >> byte;
                environment.MemoryWrite8(address + static_cast<std::uint32_t>(index),
                                         static_cast<std::uint8_t>(byte));
            }
            std::cout << "OK\n" << std::flush;
        } else if (command == "SETREG") {
            std::size_t index;
            std::uint32_t value;
            std::cin >> std::dec >> index >> std::hex >> value;
            if (index >= jit.Regs().size()) {
                return 1;
            }
            jit.Regs()[index] = value;
            std::cout << "OK\n" << std::flush;
        } else if (command == "STEP") {
            jit.Step();
            PrintState(jit);
        } else if (command == "QUIT") {
            return 0;
        } else {
            return 1;
        }
    }
    return 0;
}

int RunOneShotOracle(const std::string& cpsr_token) {
    const auto cpsr = static_cast<std::uint32_t>(std::stoul(cpsr_token, nullptr, 16));

    while (true) {
        std::array<std::uint32_t, 15> input_regs{};
        for (auto& reg : input_regs) {
            if (!(std::cin >> std::hex >> reg)) {
                return 1;
            }
        }

        std::size_t instruction_count;
        if (!(std::cin >> std::hex >> instruction_count)) {
            return 1;
        }
        std::vector<std::uint32_t> code(instruction_count);
        for (auto& instruction : code) {
            if (!(std::cin >> std::hex >> instruction)) {
                return 1;
            }
        }
        code.push_back(0xEAFFFFFE); // b .

        OracleEnvironment environment{std::move(code)};
        Dynarmic::A32::UserConfig config{};
        config.callbacks = &environment;
        config.optimizations = Dynarmic::no_optimizations;
        Dynarmic::A32::Jit jit{config};
        for (std::size_t index = 0; index < input_regs.size(); ++index) {
            jit.Regs()[index] = input_regs[index];
        }
        jit.SetCpsr(cpsr);
        jit.Run();

        PrintState(jit);
        return 0;
    }
}

int main() {
    std::ios::sync_with_stdio(false);

    std::string first_token;
    if (!(std::cin >> first_token)) {
        return 0;
    }
    if (first_token == "INIT") {
        return RunPersistentOracle();
    }
    return RunOneShotOracle(first_token);
}
