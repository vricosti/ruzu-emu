use crate::backend::x64::abi;
use rxbyak::{CodeAssembler, Reg, RAX};

/// A callback represents a host function that can be called from JIT-generated code.
///
/// The JIT emitter uses callbacks to invoke host-side functions (e.g., LookupBlock,
/// AddTicks, GetTicksRemaining) during execution.
pub trait Callback {
    /// Emit a call to this callback.
    /// `setup` is called with the available ABI parameter registers so the caller
    /// can set up arguments before the call instruction.
    fn emit_call(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()>;

    /// Emit a call where the first parameter is a return pointer.
    /// `setup` is called with (return_pointer_reg, remaining_param_regs).
    fn emit_call_with_return_pointer(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, Reg, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()>;

    /// Emit a simple call with no setup.
    fn emit_call_simple(&self, code: &mut CodeAssembler) -> rxbyak::Result<()> {
        self.emit_call(code, &|_, _| Ok(()))
    }
}

/// A simple callback wrapping a raw function pointer.
///
/// On System V ABI, passes up to 4 parameters via RDI, RSI, RDX, RCX.
pub struct SimpleCallback {
    fn_ptr: u64,
}

impl SimpleCallback {
    pub fn new(fn_ptr: u64) -> Self {
        Self { fn_ptr }
    }
}

impl Callback for SimpleCallback {
    fn emit_call(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()> {
        let params: Vec<Reg> = abi::ABI_PARAMS
            .iter()
            .take(4)
            .map(|h| h.to_reg64())
            .collect();
        setup(code, &params)?;
        emit_call_to(code, self.fn_ptr)
    }

    fn emit_call_with_return_pointer(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, Reg, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()> {
        let param1 = abi::ABI_PARAMS[0].to_reg64();
        let remaining: Vec<Reg> = abi::ABI_PARAMS
            .iter()
            .skip(1)
            .take(3)
            .map(|h| h.to_reg64())
            .collect();
        setup(code, param1, &remaining)?;
        emit_call_to(code, self.fn_ptr)
    }
}

/// A callback that prepends a fixed u64 argument as the first parameter.
///
/// Useful for passing context pointers (e.g., `this` in C++ callbacks).
pub struct ArgCallback {
    fn_ptr: u64,
    arg: u64,
}

impl ArgCallback {
    pub fn new(fn_ptr: u64, arg: u64) -> Self {
        Self { fn_ptr, arg }
    }
}

impl Callback for ArgCallback {
    fn emit_call(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()> {
        // User gets params 2-4, we fill param 1 with the fixed arg
        let remaining: Vec<Reg> = abi::ABI_PARAMS
            .iter()
            .skip(1)
            .take(3)
            .map(|h| h.to_reg64())
            .collect();
        setup(code, &remaining)?;
        let param1 = abi::ABI_PARAMS[0].to_reg64();
        code.mov(param1, self.arg as i64)?;
        emit_call_to(code, self.fn_ptr)
    }

    fn emit_call_with_return_pointer(
        &self,
        code: &mut CodeAssembler,
        setup: &dyn Fn(&mut CodeAssembler, Reg, &[Reg]) -> rxbyak::Result<()>,
    ) -> rxbyak::Result<()> {
        #[cfg(all(target_os = "windows", target_env = "msvc"))]
        {
            // MSVC x64: fixed arg in param1 (RCX), return pointer in param2 (RDX).
            let ret_ptr_reg = abi::ABI_PARAMS[1].to_reg64();
            let remaining: Vec<Reg> = abi::ABI_PARAMS
                .iter()
                .skip(2)
                .take(2)
                .map(|h| h.to_reg64())
                .collect();
            setup(code, ret_ptr_reg, &remaining)?;
            let param1 = abi::ABI_PARAMS[0].to_reg64();
            code.mov(param1, self.arg as i64)?;
            return emit_call_to(code, self.fn_ptr);
        }

        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        let ret_ptr_reg = abi::ABI_PARAMS[0].to_reg64();
        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        let remaining: Vec<Reg> = abi::ABI_PARAMS
            .iter()
            .skip(2)
            .take(2)
            .map(|h| h.to_reg64())
            .collect();
        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        setup(code, ret_ptr_reg, &remaining)?;
        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        let param2 = abi::ABI_PARAMS[1].to_reg64();
        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        code.mov(param2, self.arg as i64)?;
        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        emit_call_to(code, self.fn_ptr)
    }
}

/// Emit a call to an absolute address.
/// Uses direct `call rel32` if within range, otherwise loads into RAX first.
fn emit_call_to(code: &mut CodeAssembler, address: u64) -> rxbyak::Result<()> {
    // Always use indirect (mov rax, imm64; call rax) — easier than RIP-relative.
    code.mov(RAX, address as i64)?;
    // `BlockOfCode`'s dispatcher prologue already aligns RSP and, on Windows,
    // reserves ABI shadow space. Match upstream `BlockOfCode::CallFunction`:
    // emit the call directly without moving RSP or creating a nested frame.
    code.call_reg(RAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_callback_creation() {
        let cb = SimpleCallback::new(0xDEAD_BEEF);
        assert_eq!(cb.fn_ptr, 0xDEAD_BEEF);
    }

    #[test]
    fn test_arg_callback_creation() {
        let cb = ArgCallback::new(0xCAFE_BABE, 42);
        assert_eq!(cb.fn_ptr, 0xCAFE_BABE);
        assert_eq!(cb.arg, 42);
    }

    #[test]
    fn callback_call_preserves_dispatcher_stack_frame() {
        let mut code = CodeAssembler::new(4096).unwrap();
        SimpleCallback::new(0x0123_4567_89AB_CDEF)
            .emit_call_simple(&mut code)
            .unwrap();

        let bytes = unsafe { std::slice::from_raw_parts(code.top(), code.size()) };
        assert_eq!(
            bytes,
            &[0x48, 0xB8, 0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, 0x48, 0xFF, 0xD0,]
        );
    }

    #[test]
    fn arg_callback_exposes_parameter_after_fixed_context() {
        use std::cell::Cell;

        let mut code = CodeAssembler::new(4096).unwrap();
        let callback_parameter = Cell::new(u8::MAX);
        ArgCallback::new(0xCAFE_BABE, 42)
            .emit_call(&mut code, &|_, params| {
                callback_parameter.set(params[0].index());
                Ok(())
            })
            .unwrap();

        assert_eq!(
            callback_parameter.get(),
            abi::ABI_PARAMS[1].to_reg64().index()
        );
    }

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    #[test]
    fn arg_callback_uses_msvc_return_pointer_order() {
        use std::cell::Cell;

        let mut code = CodeAssembler::new(4096).unwrap();
        let return_register = Cell::new(u8::MAX);
        ArgCallback::new(0xCAFE_BABE, 42)
            .emit_call_with_return_pointer(&mut code, &|_, ret, _| {
                return_register.set(ret.index());
                Ok(())
            })
            .unwrap();

        assert_eq!(return_register.get(), abi::ABI_PARAMS[1].to_reg64().index());
    }
}
