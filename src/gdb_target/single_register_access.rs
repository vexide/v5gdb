use gdbstub::target::{
    TargetError, TargetResult, ext::base::single_register_access::SingleRegisterAccess,
};

use crate::{gdb_target::{V5Target, arch::ArmRegisterID}};

impl SingleRegisterAccess<()> for V5Target {
    fn read_register(
        &mut self,
        _tid: (),
        reg_id: ArmRegisterID,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        macro_rules! read_reg {
            ($buf:ident, $num:expr) => {
                {
                    let bytes = $num.to_le_bytes();
                    $buf.copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
            };
        }

        if let Some(ctx) = &mut self.exception_ctx {
            match reg_id {
                ArmRegisterID::Gpr(rid) => {
                    let Some(reg) = ctx.registers.get(rid as usize).copied() else {
                        return Err(TargetError::NonFatal);
                    };
                    read_reg!(buf, reg)
                },
                ArmRegisterID::Sp => read_reg!(buf, ctx.stack_pointer),
                ArmRegisterID::Lr => read_reg!(buf, ctx.link_register),
                ArmRegisterID::Pc => read_reg!(buf, ctx.program_counter),
                ArmRegisterID::Cpsr => read_reg!(buf, ctx.spsr.0),
                _ => Err(TargetError::NonFatal),
            }
        } else {
            Err(TargetError::NonFatal)
        }
    }

    fn write_register(
        &mut self,
        _tid: (),
        reg_id: ArmRegisterID,
        val: &[u8],
    ) -> TargetResult<(), Self> {
        macro_rules! write_reg {
            ($reg:expr, $ty:ty, $val:expr) => {
                {
                    let Ok(bytes) = $val.try_into() else {
                        return Err(TargetError::NonFatal);
                    };

                    *$reg = <$ty>::from_le_bytes(bytes);
                }
            };
        }

        if let Some(ctx) = &mut self.exception_ctx {
            match reg_id {
                ArmRegisterID::Gpr(rid) => {
                    let Some(reg) = ctx.registers.get_mut(rid as usize) else {
                        return Err(TargetError::NonFatal);
                    };
                    write_reg!(reg, u32, val)
                },
                ArmRegisterID::Sp => write_reg!(&mut ctx.stack_pointer, u32, val),
                ArmRegisterID::Lr => write_reg!(&mut ctx.link_register, u32, val),
                ArmRegisterID::Pc => write_reg!(&mut ctx.program_counter, u32, val),
                ArmRegisterID::Cpsr => write_reg!(&mut ctx.spsr.0, u32, val),
                ArmRegisterID::Fpr(rid) => {
                    let Some(reg) = ctx.vfp_registers.get_mut(rid as usize) else {
                        return Err(TargetError::NonFatal);
                    };
                    write_reg!(reg, u64, val)
                },
                ArmRegisterID::Fpscr => write_reg!(&mut ctx.fpscr, u32, val),
            }

            Ok(())
        } else {
            Err(TargetError::NonFatal)
        }
    }
}
