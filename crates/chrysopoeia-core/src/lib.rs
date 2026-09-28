//! Chrysopoeia core: the shared domain model.
//!
//! Every type that crosses a crate boundary or is sent to the web UI lives
//! here. The JSON shape of these types (serde attributes included) is the API
//! contract documented in `docs/ARCHITECTURE.md`; the TypeScript mirror lives
//! in `web/src/lib/types.ts`.

pub mod codec;
pub mod encoder;
pub mod error;
pub mod event;
pub mod hardware;
pub mod job;
pub mod library;
pub mod media;
pub mod paths;
pub mod process;
pub mod profile;
pub mod settings;
pub mod stats;
pub mod system;
pub mod validation;

pub use codec::{AudioCodec, Container, SubtitleAction, VideoCodec};
pub use encoder::{EncoderInfo, HwApi, VIDEO_ENCODERS};
pub use error::CoreError;
pub use event::{
    ActivityEntry, ActivityLevel, Event, MaxJobsSource, QueueState, ScanPhase, ScanProgress,
};
pub use hardware::{
    CpuInfo, EncoderCandidate, EncoderStatus, FfmpegInfo, GpuDevice, GpuVendor, HardwareInfo,
    HwPreference, JobRecommendation, MemoryInfo, SetupHint, SetupHintLevel,
};
pub use job::{Job, JobProgress, JobStage, JobState};
pub use library::{FileStatus, Library, LibraryStats, MediaFile};
pub use media::{ContentLight, HdrFormat, MasteringDisplay, ProbeInfo, StreamInfo, StreamKind};
pub use profile::{Goal, QualityLevel, SpeedPreset, SubtitlePolicy, TranscodeProfile};
pub use settings::{ActiveHours, OutputMode, Settings, ValidationLevel};
pub use stats::{CodecCount, Overview, SavingsPoint};
pub use system::SystemInfo;
pub use validation::{CheckStatus, ValidationCheck, ValidationReport};
