use common::common_funcs::make_magic;

pub const CURRENT_REVISION: u32 = 15;

/// The Switch 2 renderer's native revision cap. The Switch 2 audio system
/// encodes its revisions with a lowercase `v` magic (`REv`), while Switch 1
/// games keep the uppercase `REV` magic. Maps to upstream
/// `AudioCore::NativeCurrentRevision`.
pub const NATIVE_CURRENT_REVISION: u32 = 16;

/// Switch 2 renderer revisions share the `REv` magic with a lowercase `v`.
/// Maps to upstream `AudioCore::IsNativeRevision`.
pub const fn is_native_revision(user_revision: u32) -> bool {
    (user_revision & 0xffffff) == make_magic(b'R', b'E', b'v', 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportTags {
    CommandProcessingTimeEstimatorVersion4,
    CommandProcessingTimeEstimatorVersion3,
    CommandProcessingTimeEstimatorVersion2,
    MultiTapBiquadFilterProcessing,
    EffectInfoVer2,
    WaveBufferVer2,
    BiquadFilterFloatProcessing,
    VolumeMixParameterPrecisionQ23,
    MixInParameterDirtyOnlyUpdate,
    BiquadFilterEffectStateClearBugFix,
    VoicePlayedSampleCountResetAtLoopPoint,
    VoicePitchAndSrcSkipped,
    SplitterBugFix,
    FlushVoiceWaveBuffers,
    ElapsedFrameCount,
    AudioRendererVariadicCommandBufferSize,
    PerformanceMetricsDataFormatVersion2,
    AudioRendererProcessingTimeLimit80Percent,
    AudioRendererProcessingTimeLimit75Percent,
    AudioRendererProcessingTimeLimit70Percent,
    AdpcmLoopContextBugFix,
    Splitter,
    LongSizePreDelay,
    AudioUsbDeviceOutput,
    DeviceApiVersion2,
    DelayChannelMappingChange,
    ReverbChannelMappingChange,
    I3dl2ReverbChannelMappingChange,
    SplitterPrevVolumeReset,
    SplitterBiquadFilterParameter,
    SplitterDestinationV2b,
    VoiceInParameterV2,
}

const FEATURES: &[(SupportTags, u32)] = &[
    (SupportTags::AudioRendererProcessingTimeLimit70Percent, 1),
    (SupportTags::Splitter, 2),
    (SupportTags::AdpcmLoopContextBugFix, 2),
    (SupportTags::LongSizePreDelay, 3),
    (SupportTags::AudioUsbDeviceOutput, 4),
    (SupportTags::AudioRendererProcessingTimeLimit75Percent, 4),
    (SupportTags::VoicePlayedSampleCountResetAtLoopPoint, 5),
    (SupportTags::VoicePitchAndSrcSkipped, 5),
    (SupportTags::SplitterBugFix, 5),
    (SupportTags::FlushVoiceWaveBuffers, 5),
    (SupportTags::ElapsedFrameCount, 5),
    (SupportTags::AudioRendererProcessingTimeLimit80Percent, 5),
    (SupportTags::AudioRendererVariadicCommandBufferSize, 5),
    (SupportTags::PerformanceMetricsDataFormatVersion2, 5),
    (SupportTags::CommandProcessingTimeEstimatorVersion2, 5),
    (SupportTags::BiquadFilterEffectStateClearBugFix, 6),
    (SupportTags::BiquadFilterFloatProcessing, 7),
    (SupportTags::VolumeMixParameterPrecisionQ23, 7),
    (SupportTags::MixInParameterDirtyOnlyUpdate, 7),
    (SupportTags::WaveBufferVer2, 8),
    (SupportTags::CommandProcessingTimeEstimatorVersion3, 8),
    (SupportTags::EffectInfoVer2, 9),
    (SupportTags::CommandProcessingTimeEstimatorVersion4, 10),
    (SupportTags::MultiTapBiquadFilterProcessing, 10),
    (SupportTags::DelayChannelMappingChange, 11),
    (SupportTags::ReverbChannelMappingChange, 11),
    (SupportTags::I3dl2ReverbChannelMappingChange, 11),
    (SupportTags::SplitterBiquadFilterParameter, 12),
    (SupportTags::SplitterPrevVolumeReset, 13),
    (SupportTags::SplitterDestinationV2b, 15),
    (SupportTags::VoiceInParameterV2, 15),
];

/// Maps to upstream `AudioCore::GetRevisionNum`. Non-magic revisions are
/// rejected as invalid (`u32::MAX`) instead of wrapping around the magic
/// subtraction, matching the Switch 2-aware upstream check.
pub const fn get_revision_num(user_revision: u32) -> u32 {
    if user_revision < 0x100 {
        return user_revision;
    }
    if !is_native_revision(user_revision)
        && (user_revision & 0xffffff) != make_magic(b'R', b'E', b'V', 0)
    {
        return u32::MAX;
    }
    let encoded = user_revision >> 24;
    if encoded >= (b'0' as u32) {
        encoded - (b'0' as u32)
    } else {
        u32::MAX
    }
}

pub fn check_feature_supported(tag: SupportTags, user_revision: u32) -> bool {
    FEATURES
        .iter()
        .find(|(feature, _)| *feature == tag)
        .map(|(_, revision)| *revision <= get_revision_num(user_revision))
        .unwrap_or(false)
}

/// Maps to upstream `AudioCore::CheckValidRevision`. Native `REv` revisions
/// are validated against [`NATIVE_CURRENT_REVISION`], everything else against
/// [`CURRENT_REVISION`].
pub const fn check_valid_revision(user_revision: u32) -> bool {
    get_revision_num(user_revision) <= if is_native_revision(user_revision) {
        NATIVE_CURRENT_REVISION
    } else {
        CURRENT_REVISION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_12_through_15_features_match_upstream_boundaries() {
        assert!(!check_feature_supported(
            SupportTags::SplitterBiquadFilterParameter,
            11
        ));
        assert!(check_feature_supported(
            SupportTags::SplitterBiquadFilterParameter,
            12
        ));
        assert!(!check_feature_supported(
            SupportTags::SplitterPrevVolumeReset,
            12
        ));
        assert!(check_feature_supported(
            SupportTags::SplitterPrevVolumeReset,
            13
        ));
        assert!(!check_feature_supported(
            SupportTags::VoiceInParameterV2,
            14
        ));
        assert!(check_feature_supported(SupportTags::VoiceInParameterV2, 15));
        assert!(check_feature_supported(
            SupportTags::SplitterDestinationV2b,
            15
        ));
        assert!(check_valid_revision(15));
        // A bare number 16 is not the native Switch 2 encoding; only the
        // `REv`-magic form is accepted past the Switch 1 cap.
        assert!(!check_valid_revision(16));
        assert!(!check_valid_revision(u32::MAX));
    }

    #[test]
    fn native_switch2_revisions_validate_against_native_cap() {
        // OOT (Switch 2) requests `REv` revision 16, i.e.
        // 0x40764552 ('0' + 16 = '@'). Maps to upstream CheckValidRevision.
        let rev16 = make_magic(b'R', b'E', b'v', b'0' + 16);
        assert!(is_native_revision(rev16));
        assert_eq!(get_revision_num(rev16), 16);
        assert!(check_valid_revision(rev16));
        assert!(!is_native_revision(make_magic(b'R', b'E', b'V', b'5')));
        assert_eq!(
            get_revision_num(make_magic(b'R', b'E', b'V', b'5')),
            5
        );
        // Native revisions below the Switch 2 cap are valid, above are not.
        assert!(check_valid_revision(make_magic(b'R', b'E', b'v', b'7')));
        assert!(!check_valid_revision(make_magic(
            b'R',
            b'E',
            b'v',
            b'0' + 17
        )));
        // Features unlocked by earlier revisions stay unlocked for revision 16.
        assert!(check_feature_supported(SupportTags::VoiceInParameterV2, rev16));
    }
}
