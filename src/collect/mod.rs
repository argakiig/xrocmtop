//! Data collectors. Each module turns one source (sysfs, rocm-smi, vulkaninfo, /proc) into the
//! shared [`crate::model`] types. Collectors are read-only and fallible-but-total: they never
//! write device state and never panic on a missing field.

pub mod gpu_metrics;
pub mod process;
pub mod smi;
pub mod sysfs;
pub mod vulkan;
