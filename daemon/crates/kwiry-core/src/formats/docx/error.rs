// SPDX-License-Identifier: MIT OR Apache-2.0

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DocxError {
    #[error("invalid DOCX package")]
    InvalidPackage,
    #[error("DOCX package integrity verification failed")]
    IntegrityFailed,
    #[error("required DOCX package part is invalid")]
    RequiredPartInvalid,
    #[error("DOCX package limit exceeded")]
    PackageLimitExceeded,
    #[error("DOCX XML limit exceeded")]
    XmlLimitExceeded,
    #[error("DOCX contains a forbidden XML construct")]
    ForbiddenXmlConstruct,
    #[error("DOCX package is encrypted")]
    EncryptedPackage,
    #[error("DOCX part uses unsupported compression")]
    UnsupportedCompression,
    #[error("DOCX XML encoding is unsupported or malformed")]
    UnsupportedXmlEncoding,
}

impl DocxError {
    pub(crate) const fn notice_code(self) -> &'static str {
        match self {
            Self::InvalidPackage => "invalid_docx_package",
            Self::IntegrityFailed => "docx_integrity_failed",
            Self::RequiredPartInvalid => "docx_required_part_invalid",
            Self::PackageLimitExceeded => "docx_package_limit_exceeded",
            Self::XmlLimitExceeded => "docx_xml_limit_exceeded",
            Self::ForbiddenXmlConstruct => "docx_forbidden_xml_construct",
            Self::EncryptedPackage => "docx_encrypted_package",
            Self::UnsupportedCompression => "docx_unsupported_compression",
            Self::UnsupportedXmlEncoding => "docx_unsupported_xml_encoding",
        }
    }
}
