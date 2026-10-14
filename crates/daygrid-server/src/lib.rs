//! Daygrid 自托管协同服务端。
//!
//! - [`db`]：存储与同步逻辑（成员、邀请、字段级合并、增量拉取），不依赖 HTTP；
//! - [`app`]：HTTP / SSE 路由与在线状态。
pub mod app;
pub mod db;
