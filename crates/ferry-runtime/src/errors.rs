//! Turning every core error into one [`FerryError`].
//!
//! Each core error type gets one function here, and each function is one
//! `match` with no wildcard arm. Adding a variant in `ferry-core` therefore
//! fails to compile here, which is the point. A silent fall through to a
//! generic code would mean a person reads the wrong words.
//!
//! # Which code a wrapper uses
//!
//! Several core errors wrap another error. The rule is one line long. A
//! wrapper unwraps when its own row in `design/errors.json` says nothing the
//! inner row does not already say. A wrapper keeps its own code when the row
//! names the part of Ferry that failed, which the inner row cannot know.
//!
//! So `RpcError::Remote(op)` becomes the `OpError` code, because the peer's
//! refusal is the whole story. But `TransferError::Local(op)` keeps its own
//! code, because "this device could not write the file" is the fact the
//! person needs, and the `OpError` rows are written about the other device.
//!
//! # Why this module is public
//!
//! The three mappings for `AdbError`, `DiscoveryError` and `ChunkSizeError`
//! have no caller inside this crate today. A missing `adb` binary, a network
//! that refuses multicast, and a chunk size chosen in code are all handled
//! where they happen, and none of them reaches the app as an error. The
//! mappings still exist, because a new variant in one of those enums must
//! fail to compile here rather than reach a person as a missing row. Making
//! the module public is what keeps them, without an allow attribute.
//!
//! # Detail
//!
//! Two rows in `design/errors.json` use `{detail}`. `Runtime::BadConfig`'s
//! detail is a whole sentence, because the row puts it where the "why" line
//! goes. `TransferError::ChunkFailedVerification`'s detail is the failing
//! chunk's index, as digits, per `docs/engine-contract.md` item 16a. Every
//! other code carries no detail.

use ferry_core::adb::AdbError;
use ferry_core::chunk::{ChunkSizeError, ManifestError};
use ferry_core::discovery::DiscoveryError;
use ferry_core::frame::FrameError;
use ferry_core::noise::NoiseError;
use ferry_core::offer::PairingError;
use ferry_core::ops::OpError;
use ferry_core::path::PathError;
use ferry_core::peers::PeerError;
use ferry_core::roots::RootsError;
use ferry_core::rpc::RpcError;
use ferry_core::session::TransferError;
use ferry_core::tcp::TcpError;
use ferry_core::version::VersionError;
use ferry_core::wire::WireError;

use crate::FerryError;

/// Build an error that carries a code and nothing else.
pub(crate) fn failed(code: &str) -> FerryError {
    FerryError::Failed {
        code: code.to_owned(),
        detail: None,
    }
}

/// Build an error that carries a code and a detail.
///
/// Use this only for a code whose row in `design/errors.json` holds
/// `{detail}`.
pub(crate) fn failed_with(code: &str, detail: &str) -> FerryError {
    FerryError::Failed {
        code: code.to_owned(),
        detail: Some(detail.to_owned()),
    }
}

/// Ferry could not start, and this sentence says which part failed.
pub(crate) fn bad_config(detail: &str) -> FerryError {
    failed_with("Runtime::BadConfig", detail)
}

/// A push has nowhere to land on `peer_name`, because that device shares no
/// folder at all.
pub(crate) fn peer_shares_nothing(peer_name: &str) -> FerryError {
    failed_with("Runtime::PeerSharesNothing", peer_name)
}

/// The code for a decoding failure.
#[must_use]
pub fn from_wire(error: &WireError) -> FerryError {
    failed(match error {
        WireError::UnexpectedEnd => "WireError::UnexpectedEnd",
        WireError::TooLong => "WireError::TooLong",
        WireError::NotUtf8 => "WireError::NotUtf8",
        WireError::UnknownTag(_) => "WireError::UnknownTag",
        WireError::TrailingBytes => "WireError::TrailingBytes",
        WireError::InvalidPath => "WireError::InvalidPath",
        WireError::BadManifest => "WireError::BadManifest",
    })
}

/// The code for a path that failed validation.
#[must_use]
pub fn from_path(error: PathError) -> FerryError {
    failed(match error {
        PathError::Empty => "PathError::Empty",
        PathError::Absolute => "PathError::Absolute",
        PathError::ParentComponent => "PathError::ParentComponent",
        PathError::CurrentComponent => "PathError::CurrentComponent",
        PathError::NulByte => "PathError::NulByte",
        PathError::Backslash => "PathError::Backslash",
        PathError::TooLong => "PathError::TooLong",
    })
}

/// The code for a refused file operation.
#[must_use]
pub fn from_op(error: OpError) -> FerryError {
    failed(match error {
        OpError::NotFound => "OpError::NotFound",
        OpError::NotADirectory => "OpError::NotADirectory",
        OpError::IsADirectory => "OpError::IsADirectory",
        OpError::NotEmpty => "OpError::NotEmpty",
        OpError::AlreadyExists => "OpError::AlreadyExists",
        OpError::PermissionDenied => "OpError::PermissionDenied",
        OpError::InvalidPath => "OpError::InvalidPath",
        OpError::RangeTooLarge => "OpError::RangeTooLarge",
        OpError::Unsupported => "OpError::Unsupported",
        OpError::Internal => "OpError::Internal",
    })
}

/// The code for a frame layer failure.
#[must_use]
pub fn from_frame(error: &FrameError) -> FerryError {
    match error {
        FrameError::Io(_) => failed("FrameError::Io"),
        FrameError::PayloadTooLarge(_) => failed("FrameError::PayloadTooLarge"),
        FrameError::UnknownKind(_) => failed("FrameError::UnknownKind"),
        // The inner code names the field that failed, which this one cannot.
        FrameError::Wire(inner) => from_wire(inner),
    }
}

/// The code for a failed call across a connection.
#[must_use]
pub fn from_rpc(error: &RpcError) -> FerryError {
    match error {
        RpcError::Frame(inner) => from_frame(inner),
        RpcError::Wire(inner) => from_wire(inner),
        // A refusal by the peer is the whole story, so the peer's code wins.
        RpcError::Remote(inner) => from_op(*inner),
        RpcError::MismatchedRequestId { .. } => failed("RpcError::MismatchedRequestId"),
        RpcError::UnexpectedFrameKind(_) => failed("RpcError::UnexpectedFrameKind"),
        RpcError::BadName => failed("RpcError::BadName"),
    }
}

/// The code for a failed Noise handshake.
#[must_use]
pub fn from_noise(error: &NoiseError) -> FerryError {
    match error {
        // The inner code names the field that failed, which this one cannot.
        NoiseError::BadHello(inner) => from_rpc(inner),
        NoiseError::Io(_) => failed("NoiseError::Io"),
        NoiseError::Crypto(_) => failed("NoiseError::Crypto"),
        NoiseError::BadPattern => failed("NoiseError::BadPattern"),
        NoiseError::BadKeyLength => failed("NoiseError::BadKeyLength"),
        NoiseError::CommitmentMismatch => failed("NoiseError::CommitmentMismatch"),
        NoiseError::HandshakeMessageTooLarge(_) => failed("NoiseError::HandshakeMessageTooLarge"),
        NoiseError::BadHandshakePayload => failed("NoiseError::BadHandshakePayload"),
        NoiseError::MissingPeerKey => failed("NoiseError::MissingPeerKey"),
        NoiseError::UnknownOffer => failed("NoiseError::UnknownOffer"),
    }
}

/// The code for a failed version exchange.
#[must_use]
pub fn from_version(error: &VersionError) -> FerryError {
    failed(match error {
        VersionError::Io(_) => "VersionError::Io",
        VersionError::NotFerry(_) => "VersionError::NotFerry",
        VersionError::NoSharedVersion { .. } => "VersionError::NoSharedVersion",
        VersionError::UnknownMode(_) => "VersionError::UnknownMode",
    })
}

/// The code for a refused QR pairing offer.
#[must_use]
pub fn from_offer(error: PairingError) -> FerryError {
    failed(match error {
        PairingError::OfferExpired => "PairingError::OfferExpired",
        PairingError::OfferNotFerry => "PairingError::OfferNotFerry",
        PairingError::AlreadyPaired => "PairingError::AlreadyPaired",
        // Never raised by this crate; kept here so the match stays
        // exhaustive if the app ever routes a camera refusal through this
        // same function instead of building the `FerryError` itself.
        PairingError::CameraRefused => "PairingError::CameraRefused",
    })
}

/// The code for a connection that never became a usable channel.
#[must_use]
pub fn from_tcp(error: &TcpError) -> FerryError {
    match error {
        TcpError::Io(_) => failed("TcpError::Io"),
        // Both inner types say more than "the connection was refused". The
        // pairing screen depends on this: a commitment mismatch has to reach
        // the person as itself, not as a generic refusal.
        TcpError::Version(inner) => from_version(inner),
        TcpError::Noise(inner) => from_noise(inner),
        TcpError::TooManyPending => failed("TcpError::TooManyPending"),
        TcpError::TooManyPendingFromAddr => failed("TcpError::TooManyPendingFromAddr"),
        TcpError::Timeout => failed("TcpError::Timeout"),
    }
}

/// The code for a chunk size the hash tree will not accept.
#[must_use]
pub fn from_chunk_size(error: ChunkSizeError) -> FerryError {
    failed(match error {
        ChunkSizeError::NotPowerOfTwo => "ChunkSizeError::NotPowerOfTwo",
        ChunkSizeError::TooSmall => "ChunkSizeError::TooSmall",
        ChunkSizeError::TooLarge => "ChunkSizeError::TooLarge",
    })
}

/// The code for a stored manifest that does not agree with itself.
#[must_use]
pub fn from_manifest(error: ManifestError) -> FerryError {
    failed(match error {
        // This stays here rather than unwrapping to the `WireError` code.
        // The `WireError` rows describe a message on the network. This is a
        // file on disk, and the row says so.
        ManifestError::Wire(_) => "ManifestError::Wire",
        ManifestError::BadChunkSize(_) => "ManifestError::BadChunkSize",
        ManifestError::WrongChunkCount { .. } => "ManifestError::WrongChunkCount",
        ManifestError::TooManyChunks => "ManifestError::TooManyChunks",
        ManifestError::RootMismatch => "ManifestError::RootMismatch",
    })
}

/// The code for a transfer that stopped.
#[must_use]
pub fn from_transfer(error: &TransferError) -> FerryError {
    match error {
        TransferError::Rpc(inner) => from_rpc(inner),
        // Kept, not unwrapped. This says local storage failed, and the
        // `OpError` rows are written about the other device.
        TransferError::Local(_) => failed("TransferError::Local"),
        TransferError::Record(inner) => from_manifest(*inner),
        TransferError::BadPath(_) => failed("TransferError::BadPath"),
        // The failing chunk's index is the one fact the code alone cannot
        // carry, so it travels as the detail. docs/engine-contract.md item
        // 16a.
        TransferError::ChunkFailedVerification { index } => {
            failed_with("TransferError::ChunkFailedVerification", &index.to_string())
        }
        TransferError::ShortRead { .. } => failed("TransferError::ShortRead"),
        TransferError::NoRandomness => failed("TransferError::NoRandomness"),
    }
}

/// The code for a failure of the paired device list.
#[must_use]
pub fn from_peer(error: &PeerError) -> FerryError {
    failed(match error {
        PeerError::Io(_) => "PeerError::Io",
        // Kept, not unwrapped, for the same reason as `TransferError::Local`.
        // These rows name the device list, which the inner rows cannot.
        PeerError::Wire(_) => "PeerError::Wire",
        PeerError::BadKey(_) => "PeerError::BadKey",
        PeerError::UnknownFormat(_) => "PeerError::UnknownFormat",
        PeerError::TooMany => "PeerError::TooMany",
        PeerError::NameTooLong => "PeerError::NameTooLong",
        PeerError::NoRandomness => "PeerError::NoRandomness",
    })
}

/// The code for a set of roots that could not be opened.
#[must_use]
pub fn from_roots(error: RootsError) -> FerryError {
    failed(match error {
        RootsError::RootNameInvalid => "RootsError::RootNameInvalid",
        RootsError::RootNameTaken => "RootsError::RootNameTaken",
        RootsError::RootNotAFolder => "RootsError::RootNotAFolder",
        RootsError::RootOverlaps => "RootsError::RootOverlaps",
        RootsError::NoRoots => "RootsError::NoRoots",
    })
}

/// The code for a discovery failure.
#[must_use]
pub fn from_discovery(error: &DiscoveryError) -> FerryError {
    failed(match error {
        DiscoveryError::Mdns(_) => "DiscoveryError::Mdns",
        DiscoveryError::NoRandomness => "DiscoveryError::NoRandomness",
    })
}

/// The code for a failed call to `adb`.
#[must_use]
pub fn from_adb(error: &AdbError) -> FerryError {
    failed(match error {
        AdbError::NotFound => "AdbError::NotFound",
        AdbError::Io(_) => "AdbError::Io",
        AdbError::Failed { .. } => "AdbError::Failed",
        AdbError::Timeout => "AdbError::Timeout",
        AdbError::Unparseable(_) => "AdbError::Unparseable",
        AdbError::RelativeBinary(_) => "AdbError::RelativeBinary",
    })
}

#[cfg(test)]
mod tests {
    use super::{from_op, from_rpc, from_tcp, from_transfer};
    use crate::FerryError;
    use ferry_core::noise::NoiseError;
    use ferry_core::ops::OpError;
    use ferry_core::rpc::RpcError;
    use ferry_core::session::TransferError;
    use ferry_core::tcp::TcpError;

    fn code_of(error: &FerryError) -> String {
        let FerryError::Failed { code, .. } = error;
        code.clone()
    }

    #[test]
    fn a_remote_refusal_reports_the_peer_code() {
        let error = from_rpc(&RpcError::Remote(OpError::NotFound));
        assert_eq!(code_of(&error), "OpError::NotFound");
    }

    #[test]
    fn a_wrapped_remote_refusal_still_reaches_the_peer_code() {
        let error = from_transfer(&TransferError::Rpc(RpcError::Remote(OpError::IsADirectory)));
        assert_eq!(code_of(&error), "OpError::IsADirectory");
    }

    #[test]
    fn a_commitment_mismatch_reaches_the_pairing_screen_as_itself() {
        let error = from_tcp(&TcpError::Noise(NoiseError::CommitmentMismatch));
        assert_eq!(code_of(&error), "NoiseError::CommitmentMismatch");
    }

    #[test]
    fn a_local_storage_failure_keeps_the_transfer_code() {
        let error = from_transfer(&TransferError::Local(OpError::PermissionDenied));
        assert_eq!(code_of(&error), "TransferError::Local");
    }

    #[test]
    fn no_code_carries_a_detail_unless_its_row_asks_for_one() {
        let FerryError::Failed { detail, .. } = from_op(OpError::NotFound);
        assert_eq!(detail, None);
    }
}
