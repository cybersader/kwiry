/// Do not change until production Holdings enforces R17 on every effective
/// resource, including single-vault requests. Registration is not Holdings.
pub const HOLDINGS_SERVING_IMPLEMENTED: bool = false;

#[derive(Clone, Copy)]
pub enum OpenClastRoomFormat {
    PreSpec,
    Closed,
    Open,
}

impl OpenClastRoomFormat {
    #[doc(hidden)]
    pub const fn from_build_flags(room_inclusive: bool, holdings: bool) -> Self {
        match (room_inclusive, holdings) {
            (false, _) => Self::PreSpec,
            (true, false) => Self::Closed,
            (true, true) => Self::Open,
        }
    }

    pub const fn admits_repeated_vault(self) -> bool {
        matches!(self, Self::Open)
    }

    pub const fn multi_room_header(self) -> &'static str {
        if self.admits_repeated_vault() {
            "open"
        } else {
            "closed"
        }
    }

    pub const fn health_body(self) -> &'static str {
        match self {
            Self::PreSpec => r#"{"status":"ok"}"#,
            Self::Closed => r#"{"status":"ok","id_scheme":"room-v1","multi_room":"closed"}"#,
            Self::Open => r#"{"status":"ok","id_scheme":"room-v1","multi_room":"open"}"#,
        }
    }
}

/// Expand at the call site so the component-only Holdings override applies
/// to each crate's unit-test build, never to a deployable binary or library.
#[doc(hidden)]
#[macro_export]
macro_rules! openclast_room_format {
    () => {{
        #[cfg(test)]
        const HOLDINGS: bool = true;
        #[cfg(not(test))]
        const HOLDINGS: bool = $crate::HOLDINGS_SERVING_IMPLEMENTED;
        $crate::OpenClastRoomFormat::from_build_flags($crate::ROOM_INCLUSIVE_IDS, HOLDINGS)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p17_build_flag_truth_table() {
        for (room_ids, holdings, header, body) in [
            (false, false, "closed", r#"{"status":"ok"}"#),
            (false, true, "closed", r#"{"status":"ok"}"#),
            (
                true,
                false,
                "closed",
                r#"{"status":"ok","id_scheme":"room-v1","multi_room":"closed"}"#,
            ),
            (
                true,
                true,
                "open",
                r#"{"status":"ok","id_scheme":"room-v1","multi_room":"open"}"#,
            ),
        ] {
            let format = OpenClastRoomFormat::from_build_flags(room_ids, holdings);
            assert_eq!(format.health_body(), body);
            assert_eq!(format.multi_room_header(), header);
            assert_eq!(format.admits_repeated_vault(), header == "open");
        }
    }
}
