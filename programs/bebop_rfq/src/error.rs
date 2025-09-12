use anchor_lang::error_code;

#[error_code]
pub enum BebopError {
    ZeroTakerAmount,
    ZeroMakerAmount,
    WrongSharedAccountAddress,
    MissingTemporaryWrappedSolTokenAccount,
    Token2022MintExtensionNotSupported,
    OrderExpired,
    InvalidNativeTokenAddress,
    InvalidOutputAmount
}
