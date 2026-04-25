use thiserror::Error;

/// Errors from book operations.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookError {
    /// Order ID not found on the book.
    #[error("order not found")]
    UnknownOrderId,

    /// Order ID already exists on the book.
    #[error("order already exists")]
    DuplicateOrderId,

    /// Sequence counter would overflow.
    #[error("sequence counter overflow")]
    SeqOverflow,
}
