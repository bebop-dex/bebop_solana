use anchor_lang::error_code;

#[error_code]
pub enum BebopError {
    InvalidCalculation,
    Token2022MintExtensionNotSupported,
    MissingTemporaryWrappedSolTokenAccount
}
