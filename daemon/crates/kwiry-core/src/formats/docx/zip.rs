// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use rawzip::{CompressionMethod, ZipArchive, ZipArchiveEntryWayfinder, ZipVerification};

use crate::model::MAX_FILE_BYTES;

use super::error::DocxError;
use super::limits::{
    DECOMPRESSION_BUFFER_BYTES, EXPANSION_RATIO_ALLOWANCE_BYTES, MAX_CENTRAL_DIRECTORY_ENTRIES,
    MAX_DECLARED_ENTRY_BYTES, MAX_DECLARED_PACKAGE_BYTES, MAX_EXPANSION_RATIO,
    MAX_SELECTED_XML_PART_BYTES, MAX_SELECTED_XML_TOTAL_BYTES, MAX_ZIP_METADATA_BYTES,
};
use super::opc::canonicalize_zip_name;

#[derive(Debug, Clone)]
pub(super) struct ArchiveEntry {
    pub(super) compression_method: CompressionMethod,
    pub(super) compressed_size: u64,
    pub(super) uncompressed_size: u64,
    wayfinder: ZipArchiveEntryWayfinder,
}

#[derive(Debug)]
pub(super) struct ArchiveInventory<'a> {
    archive: rawzip::ZipSliceArchive<&'a [u8]>,
    entries: BTreeMap<String, ArchiveEntry>,
    selected_uncompressed: u64,
}

impl<'a> ArchiveInventory<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Result<Self, DocxError> {
        if bytes.len() > MAX_FILE_BYTES as usize {
            return Err(DocxError::PackageLimitExceeded);
        }
        let archive = ZipArchive::from_slice(bytes).map_err(|_| DocxError::InvalidPackage)?;
        if archive.entries_hint() > MAX_CENTRAL_DIRECTORY_ENTRIES as u64
            || archive.end_offset() > bytes.len() as u64
        {
            return Err(DocxError::PackageLimitExceeded);
        }

        let directory_offset = archive.directory_offset();
        let mut entries = BTreeMap::new();
        let mut folded_uris = BTreeSet::new();
        let mut local_offsets = BTreeSet::new();
        let mut ranges = Vec::new();
        let mut metadata_bytes = archive.comment().as_bytes().len();
        let mut declared_total = 0_u64;
        let mut count = 0_usize;
        let mut central_entries = archive.entries();

        while let Some(central) = central_entries
            .next_entry()
            .map_err(|_| DocxError::InvalidPackage)?
        {
            count = count
                .checked_add(1)
                .ok_or(DocxError::PackageLimitExceeded)?;
            if count > MAX_CENTRAL_DIRECTORY_ENTRIES {
                return Err(DocxError::PackageLimitExceeded);
            }

            let central_path = central.file_path();
            let source_name_bytes = central_path.as_ref();
            let source_name =
                std::str::from_utf8(source_name_bytes).map_err(|_| DocxError::InvalidPackage)?;
            let part_uri = canonicalize_zip_name(source_name, central.is_dir())?;
            let folded_uri = part_uri.to_ascii_lowercase();
            if entries.contains_key(&part_uri) || !folded_uris.insert(folded_uri) {
                return Err(DocxError::InvalidPackage);
            }
            if !local_offsets.insert(central.local_header_offset()) {
                return Err(DocxError::InvalidPackage);
            }

            let compressed_size = central.compressed_size_hint();
            let uncompressed_size = central.uncompressed_size_hint();
            if compressed_size > MAX_DECLARED_ENTRY_BYTES
                || uncompressed_size > MAX_DECLARED_ENTRY_BYTES
            {
                return Err(DocxError::PackageLimitExceeded);
            }
            declared_total = declared_total
                .checked_add(uncompressed_size)
                .filter(|total| *total <= MAX_DECLARED_PACKAGE_BYTES)
                .ok_or(DocxError::PackageLimitExceeded)?;

            metadata_bytes = metadata_bytes
                .checked_add(source_name_bytes.len())
                .and_then(|total| total.checked_add(central.extra_fields().remaining_bytes().len()))
                .and_then(|total| total.checked_add(central.comment().as_bytes().len()))
                .filter(|total| *total <= MAX_ZIP_METADATA_BYTES)
                .ok_or(DocxError::PackageLimitExceeded)?;

            let flags = central.flags();
            let compression_method = central.compression_method();
            if flags.is_encrypted()
                || flags.has_strong_encryption()
                || flags.is_masked()
                || compression_method == CompressionMethod::AES
            {
                return Err(DocxError::EncryptedPackage);
            }

            let wayfinder = central.wayfinder();
            let local = archive
                .get_entry(wayfinder)
                .map_err(|_| DocxError::InvalidPackage)?;
            let local_header = local.local_header();
            metadata_bytes = metadata_bytes
                .checked_add(local_header.file_path().as_ref().len())
                .and_then(|total| {
                    total.checked_add(local_header.extra_fields().remaining_bytes().len())
                })
                .filter(|total| *total <= MAX_ZIP_METADATA_BYTES)
                .ok_or(DocxError::PackageLimitExceeded)?;

            if local_header.file_path().as_ref() != source_name_bytes
                || local_header.compression_method() != compression_method
                || local_header.flags().bits() != flags.bits()
            {
                return Err(DocxError::InvalidPackage);
            }
            if local_header.flags().is_encrypted()
                || local_header.flags().has_strong_encryption()
                || local_header.flags().is_masked()
            {
                return Err(DocxError::EncryptedPackage);
            }

            if flags.has_data_descriptor() {
                if (local_header.crc32() != 0 && local_header.crc32() != central.crc32())
                    || (local_header.compressed_size_hint() != 0
                        && local_header.compressed_size_hint() != compressed_size)
                    || (local_header.uncompressed_size_hint() != 0
                        && local_header.uncompressed_size_hint() != uncompressed_size)
                {
                    return Err(DocxError::IntegrityFailed);
                }
                let descriptor = local
                    .data_descriptor()
                    .map_err(|_| DocxError::IntegrityFailed)?
                    .ok_or(DocxError::IntegrityFailed)?;
                if descriptor.crc32() != central.crc32()
                    || descriptor.compressed_size() != compressed_size
                    || descriptor.uncompressed_size() != uncompressed_size
                {
                    return Err(DocxError::IntegrityFailed);
                }
            } else if local_header.crc32() != central.crc32()
                || local_header.compressed_size_hint() != compressed_size
                || local_header.uncompressed_size_hint() != uncompressed_size
            {
                return Err(DocxError::IntegrityFailed);
            }

            if local.data().len() as u64 != compressed_size {
                return Err(DocxError::IntegrityFailed);
            }
            let (range_start, range_end) = local.compressed_data_range();
            if range_start > range_end
                || range_end > bytes.len() as u64
                || range_end > directory_offset
                || central.local_header_offset() >= directory_offset
            {
                return Err(DocxError::InvalidPackage);
            }
            if range_start != range_end {
                ranges.push((range_start, range_end));
            }

            entries.insert(
                part_uri.clone(),
                ArchiveEntry {
                    compression_method,
                    compressed_size,
                    uncompressed_size,
                    wayfinder,
                },
            );
        }

        if count != archive.entries_hint() as usize {
            return Err(DocxError::InvalidPackage);
        }
        ranges.sort_unstable_by_key(|range| range.0);
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(DocxError::InvalidPackage);
        }

        Ok(Self {
            archive,
            entries,
            selected_uncompressed: 0,
        })
    }

    pub(super) fn contains(&self, part_uri: &str) -> bool {
        self.entries.contains_key(part_uri)
    }

    pub(super) fn open_selected_xml(&mut self, part_uri: &str) -> Result<Vec<u8>, DocxError> {
        let entry = self
            .entries
            .get(part_uri)
            .cloned()
            .ok_or(DocxError::RequiredPartInvalid)?;
        if entry.uncompressed_size > MAX_SELECTED_XML_PART_BYTES {
            return Err(DocxError::PackageLimitExceeded);
        }
        let ratio_limit = entry
            .compressed_size
            .checked_mul(MAX_EXPANSION_RATIO)
            .and_then(|value| value.checked_add(EXPANSION_RATIO_ALLOWANCE_BYTES))
            .ok_or(DocxError::PackageLimitExceeded)?;
        if entry.uncompressed_size > ratio_limit {
            return Err(DocxError::PackageLimitExceeded);
        }
        let selected_total = self
            .selected_uncompressed
            .checked_add(entry.uncompressed_size)
            .filter(|total| *total <= MAX_SELECTED_XML_TOTAL_BYTES)
            .ok_or(DocxError::PackageLimitExceeded)?;

        let local = self
            .archive
            .get_entry(entry.wayfinder)
            .map_err(|_| DocxError::IntegrityFailed)?;
        let compressed = local.data();
        if compressed.len() as u64 != entry.compressed_size {
            return Err(DocxError::IntegrityFailed);
        }

        let capacity = usize::try_from(entry.uncompressed_size)
            .map_err(|_| DocxError::PackageLimitExceeded)?;
        let mut output = Vec::with_capacity(capacity);
        let mut crc = rawzip::Crc32::new();
        match entry.compression_method {
            CompressionMethod::STORE => {
                if compressed.len() as u64 != entry.uncompressed_size {
                    return Err(DocxError::IntegrityFailed);
                }
                for chunk in compressed.chunks(DECOMPRESSION_BUFFER_BYTES) {
                    crc.update(chunk);
                    output.extend_from_slice(chunk);
                }
            }
            CompressionMethod::DEFLATE => {
                let mut decoder = flate2::bufread::DeflateDecoder::new(compressed);
                let mut buffer = [0_u8; DECOMPRESSION_BUFFER_BYTES];
                loop {
                    let read = decoder
                        .read(&mut buffer)
                        .map_err(|_| DocxError::IntegrityFailed)?;
                    if read == 0 {
                        break;
                    }
                    let next_len = output
                        .len()
                        .checked_add(read)
                        .filter(|len| *len as u64 <= entry.uncompressed_size)
                        .ok_or(DocxError::IntegrityFailed)?;
                    crc.update(&buffer[..read]);
                    output.extend_from_slice(&buffer[..read]);
                    debug_assert_eq!(output.len(), next_len);
                }
                if decoder.total_in() != compressed.len() as u64 {
                    return Err(DocxError::IntegrityFailed);
                }
            }
            _ => return Err(DocxError::UnsupportedCompression),
        }

        entry
            .wayfinder
            .uncompressed_size_hint()
            .eq(&(output.len() as u64))
            .then_some(())
            .ok_or(DocxError::IntegrityFailed)?;
        local
            .claim_verifier()
            .valid(ZipVerification {
                crc: crc.checksum(),
                uncompressed_size: output.len() as u64,
            })
            .map_err(|_| DocxError::IntegrityFailed)?;
        self.selected_uncompressed = selected_total;
        Ok(output)
    }
}
