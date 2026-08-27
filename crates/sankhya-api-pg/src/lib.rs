//! The PostgreSQL wire protocol front door.

#![doc(html_root_url = "https://docs.rs/sankhya-api-pg")]

pub mod catalog;
pub mod message;

pub use catalog::{answer, recognise, CatalogQuery, CatalogResult, CatalogTable, Unsupported};
pub use message::{
    decode, decode_startup, encode, BackendMessage, DecodeError, FieldDescription, FrontendMessage,
};
