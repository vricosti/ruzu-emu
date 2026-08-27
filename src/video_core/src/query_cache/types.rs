// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/types.h`.
//!
//! Defines query types, comparison modes, reduction operations, and property flags
//! used throughout the query cache subsystem.

use bitflags::bitflags;

bitflags! {
    /// Property flags for query operations.
    ///
    /// Maps to C++ `QueryPropertiesFlags`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct QueryPropertiesFlags: u32 {
        const HAS_TIMEOUT = 1 << 0;
        const IS_A_FENCE  = 1 << 1;
    }
}

/// This should always be equivalent to maxwell3d Report Semaphore Reports.
///
/// Maps to C++ `QueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum QueryType {
    /// "None" in docs, but confirmed via hardware to return the payload.
    Payload = 0,
    VerticesGenerated = 1,
    ZPassPixelCount = 2,
    PrimitivesGenerated = 3,
    AlphaBetaClocks = 4,
    VertexShaderInvocations = 5,
    StreamingPrimitivesNeededMinusSucceeded = 6,
    GeometryShaderInvocations = 7,
    GeometryShaderPrimitivesGenerated = 9,
    ZCullStats0 = 10,
    StreamingPrimitivesSucceeded = 11,
    ZCullStats1 = 12,
    StreamingPrimitivesNeeded = 13,
    ZCullStats2 = 14,
    ClipperInvocations = 15,
    ZCullStats3 = 16,
    ClipperPrimitivesGenerated = 17,
    VtgPrimitivesOut = 18,
    PixelShaderInvocations = 19,
    ZPassPixelCount64 = 21,
    IeeeCleanColorTarget = 24,
    IeeeCleanZetaTarget = 25,
    StreamingByteCount = 26,
    TessellationInitInvocations = 27,
    BoundingRectangle = 28,
    TessellationShaderInvocations = 29,
    TotalStreamingPrimitivesNeededMinusSucceeded = 30,
    TessellationShaderPrimitivesGenerated = 31,
}

/// Sentinel value representing the maximum number of query types.
pub const MAX_QUERY_TYPES: usize = 32;

/// Comparison modes for Host Conditional Rendering.
///
/// Maps to C++ `ComparisonMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ComparisonMode {
    False = 0,
    True = 1,
    Conditional = 2,
    IfEqual = 3,
    IfNotEqual = 4,
}

/// Maximum number of comparison modes.
pub const MAX_COMPARISON_MODE: usize = 5;

/// Reduction operations.
///
/// Maps to C++ `ReductionOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ReductionOp {
    RedAdd = 0,
    RedMin = 1,
    RedMax = 2,
    RedInc = 3,
    RedDec = 4,
    RedAnd = 5,
    RedOr = 6,
    RedXor = 7,
}

/// Maximum number of reduction operations.
pub const MAX_REDUCTION_OP: usize = 8;
