// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::formats::ooxml::PackageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum ExcelError {
    #[error("input is not a SpreadsheetML package")]
    NotSpreadsheet,
    #[error("invalid Excel package")]
    InvalidPackage,
    #[error("Excel package integrity verification failed")]
    IntegrityFailed,
    #[error("required Excel package part is invalid")]
    RequiredPartInvalid,
    #[error("Excel package limit exceeded")]
    PackageLimitExceeded,
    #[error("Excel XML limit exceeded")]
    XmlLimitExceeded,
    #[error("Excel contains a forbidden XML construct")]
    ForbiddenXmlConstruct,
    #[error("Excel package is encrypted")]
    EncryptedPackage,
    #[error("Excel part uses unsupported compression")]
    UnsupportedCompression,
    #[error("Excel XML encoding is unsupported or malformed")]
    UnsupportedXmlEncoding,
}

impl ExcelError {
    pub(super) const fn notice_code(self) -> &'static str {
        match self {
            Self::NotSpreadsheet => "excel_not_spreadsheet",
            Self::InvalidPackage => "invalid_excel_package",
            Self::IntegrityFailed => "excel_integrity_failed",
            Self::RequiredPartInvalid => "excel_required_part_invalid",
            Self::PackageLimitExceeded => "excel_package_limit_exceeded",
            Self::XmlLimitExceeded => "excel_xml_limit_exceeded",
            Self::ForbiddenXmlConstruct => "excel_forbidden_xml_construct",
            Self::EncryptedPackage => "excel_encrypted_package",
            Self::UnsupportedCompression => "excel_unsupported_compression",
            Self::UnsupportedXmlEncoding => "excel_unsupported_xml_encoding",
        }
    }
}

impl From<PackageError> for ExcelError {
    fn from(error: PackageError) -> Self {
        match error {
            PackageError::InvalidPackage => Self::InvalidPackage,
            PackageError::IntegrityFailed => Self::IntegrityFailed,
            PackageError::RequiredPartInvalid => Self::RequiredPartInvalid,
            PackageError::PackageLimitExceeded => Self::PackageLimitExceeded,
            PackageError::XmlLimitExceeded => Self::XmlLimitExceeded,
            PackageError::ForbiddenXmlConstruct => Self::ForbiddenXmlConstruct,
            PackageError::EncryptedPackage => Self::EncryptedPackage,
            PackageError::UnsupportedCompression => Self::UnsupportedCompression,
            PackageError::UnsupportedXmlEncoding => Self::UnsupportedXmlEncoding,
        }
    }
}
