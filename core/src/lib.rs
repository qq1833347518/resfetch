//! resfetch-core —— 资源下载器的跨平台内核。
//!
//! 设计原则（也是这个项目存在的理由）：
//! 1. 本 crate **零第三方依赖**，只依赖 `std`。三端（macOS / Windows / Android）
//!    只要各自套一层壳，就能编译同一份内核，不需要重写业务逻辑。
//! 2. 不碰网络、不碰 UI、不碰平台 API。所有 IO 由外壳注入，这样安卓上可以用
//!    JNI/UniFFI 直接调，桌面端可以直接跑 CLI。
//! 3. 每个模块都能纯函数测试，不需要真实账号、不需要联网。

pub mod aes;
pub mod cookie;
pub mod douyin;
pub mod epub;
pub mod error;
pub mod hls;
pub mod json;
pub mod link;
pub mod log;
pub mod naming;
pub mod plan;

pub use error::{Error, Result};

/// 版本号集中一处，外壳与 UI 都从这里读，避免"多处版本不一致"。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
