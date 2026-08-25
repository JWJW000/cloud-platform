//! Master 传输层适配器（V7 实施方案第 4.3 节）。

pub mod tonic;

pub use self::tonic::TonicMasterAdapter;
