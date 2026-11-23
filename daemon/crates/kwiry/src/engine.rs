//! The identity of this binary's preparation engine. `checkroom` and
//! `selfcheck` both print it from here, so a deployment can prove that the
//! checker that vetted a room and the server that serves it are one build.
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Engine {
    pub version: &'static str,
    /// Full commit the binary was built from; `None` when the build had
    /// neither git nor `KWIRY_GIT_COMMIT`/`GIT_COMMIT`.
    pub commit: Option<&'static str>,
    /// `<target triple>/<cargo profile>`.
    pub build: &'static str,
    /// The fingerprint that binds prepared sources to this build's extractors.
    pub extraction_policy_fingerprint: &'static str,
    pub source_preparation_schema_version: u32,
}

pub(crate) fn engine() -> Engine {
    let commit = env!("KWIRY_BUILD_COMMIT");
    Engine {
        version: env!("CARGO_PKG_VERSION"),
        commit: (!commit.is_empty()).then_some(commit),
        build: env!("KWIRY_BUILD_ID"),
        extraction_policy_fingerprint: kwiry_core::extraction_policy_fingerprint(),
        source_preparation_schema_version: kwiry_core::SOURCE_PREPARATION_SCHEMA_VERSION,
    }
}
