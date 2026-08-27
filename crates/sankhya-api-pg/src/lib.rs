//! The PostgreSQL wire protocol front door.

#![doc(html_root_url = "https://docs.rs/sankhya-api-pg")]

pub mod catalog;
pub mod listener;
pub mod message;
pub mod session;

pub use catalog::{answer, recognise, CatalogQuery, CatalogResult, CatalogTable, Unsupported};
pub use listener::{serve, PgListener};
pub use message::{
    decode, decode_startup, encode, BackendMessage, DecodeError, FieldDescription, FrontendMessage,
};
pub use session::{Connection, Handler, Phase, QueryFailure, QueryResult};
