/// Negative response code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nrc {
    GeneralReject,
    ServiceNotSupported,
    SubFunctionNotSupported,
    IncorrectMessageLengthOrInvalidFormat,
    ResponseTooLong,
    ConditionsNotCorrect,
    RequestSequenceError,
    NoResponseFromSubnetComponent,
    RequestOutOfRange,
    SecurityAccessDenied,
    InvalidKey,
    ExceededNumberOfAttempts,
    RequiredTimeDelayNotExpired,
    UploadDownloadNotAccepted,
    TransferDataSuspended,
    GeneralProgrammingFailure,
    WrongBlockSequenceCounter,
    ResponsePending,
    SubFunctionNotSupportedInActiveSession,
    ServiceNotSupportedInActiveSession,
    Other(u8),
}

impl Nrc {
    pub fn from_u8(v: u8) -> Nrc {
        match v {
            0x10 => Nrc::GeneralReject,
            0x11 => Nrc::ServiceNotSupported,
            0x12 => Nrc::SubFunctionNotSupported,
            0x13 => Nrc::IncorrectMessageLengthOrInvalidFormat,
            0x14 => Nrc::ResponseTooLong,
            0x22 => Nrc::ConditionsNotCorrect,
            0x24 => Nrc::RequestSequenceError,
            0x25 => Nrc::NoResponseFromSubnetComponent,
            0x31 => Nrc::RequestOutOfRange,
            0x33 => Nrc::SecurityAccessDenied,
            0x35 => Nrc::InvalidKey,
            0x36 => Nrc::ExceededNumberOfAttempts,
            0x37 => Nrc::RequiredTimeDelayNotExpired,
            0x70 => Nrc::UploadDownloadNotAccepted,
            0x71 => Nrc::TransferDataSuspended,
            0x72 => Nrc::GeneralProgrammingFailure,
            0x73 => Nrc::WrongBlockSequenceCounter,
            0x78 => Nrc::ResponsePending,
            0x7E => Nrc::SubFunctionNotSupportedInActiveSession,
            0x7F => Nrc::ServiceNotSupportedInActiveSession,
            n => Nrc::Other(n),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Nrc::GeneralReject => 0x10,
            Nrc::ServiceNotSupported => 0x11,
            Nrc::SubFunctionNotSupported => 0x12,
            Nrc::IncorrectMessageLengthOrInvalidFormat => 0x13,
            Nrc::ResponseTooLong => 0x14,
            Nrc::ConditionsNotCorrect => 0x22,
            Nrc::RequestSequenceError => 0x24,
            Nrc::NoResponseFromSubnetComponent => 0x25,
            Nrc::RequestOutOfRange => 0x31,
            Nrc::SecurityAccessDenied => 0x33,
            Nrc::InvalidKey => 0x35,
            Nrc::ExceededNumberOfAttempts => 0x36,
            Nrc::RequiredTimeDelayNotExpired => 0x37,
            Nrc::UploadDownloadNotAccepted => 0x70,
            Nrc::TransferDataSuspended => 0x71,
            Nrc::GeneralProgrammingFailure => 0x72,
            Nrc::WrongBlockSequenceCounter => 0x73,
            Nrc::ResponsePending => 0x78,
            Nrc::SubFunctionNotSupportedInActiveSession => 0x7E,
            Nrc::ServiceNotSupportedInActiveSession => 0x7F,
            Nrc::Other(n) => n,
        }
    }

    /// The ISO 14229 name of the code.
    pub fn name(self) -> &'static str {
        match self {
            Nrc::GeneralReject => "generalReject",
            Nrc::ServiceNotSupported => "serviceNotSupported",
            Nrc::SubFunctionNotSupported => "subFunctionNotSupported",
            Nrc::IncorrectMessageLengthOrInvalidFormat => "incorrectMessageLengthOrInvalidFormat",
            Nrc::ResponseTooLong => "responseTooLong",
            Nrc::ConditionsNotCorrect => "conditionsNotCorrect",
            Nrc::RequestSequenceError => "requestSequenceError",
            Nrc::NoResponseFromSubnetComponent => "noResponseFromSubnetComponent",
            Nrc::RequestOutOfRange => "requestOutOfRange",
            Nrc::SecurityAccessDenied => "securityAccessDenied",
            Nrc::InvalidKey => "invalidKey",
            Nrc::ExceededNumberOfAttempts => "exceededNumberOfAttempts",
            Nrc::RequiredTimeDelayNotExpired => "requiredTimeDelayNotExpired",
            Nrc::UploadDownloadNotAccepted => "uploadDownloadNotAccepted",
            Nrc::TransferDataSuspended => "transferDataSuspended",
            Nrc::GeneralProgrammingFailure => "generalProgrammingFailure",
            Nrc::WrongBlockSequenceCounter => "wrongBlockSequenceCounter",
            Nrc::ResponsePending => "requestCorrectlyReceivedResponsePending",
            Nrc::SubFunctionNotSupportedInActiveSession => "subFunctionNotSupportedInActiveSession",
            Nrc::ServiceNotSupportedInActiveSession => "serviceNotSupportedInActiveSession",
            Nrc::Other(_) => "unknown",
        }
    }
}
