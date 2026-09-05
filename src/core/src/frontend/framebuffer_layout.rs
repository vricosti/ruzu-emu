// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/frontend/framebuffer_layout.h and framebuffer_layout.cpp
//! Framebuffer layout calculations.

/// Minimum window size constants.
///
/// Corresponds to upstream `Layout::MinimumSize`.
pub mod minimum_size {
    pub const WIDTH: u32 = 640;
    pub const HEIGHT: u32 = 360;
}

/// Undocked screen resolution constants.
///
/// Corresponds to upstream `Layout::ScreenUndocked`.
pub mod screen_undocked {
    pub const WIDTH: u32 = 1280;
    pub const HEIGHT: u32 = 720;
}

/// Docked screen resolution constants.
///
/// Corresponds to upstream `Layout::ScreenDocked`.
pub mod screen_docked {
    pub const WIDTH: u32 = 1920;
    pub const HEIGHT: u32 = 1080;
}

/// Aspect ratio presets.
///
/// Corresponds to upstream `Layout::AspectRatio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AspectRatio {
    Default = 0,
    R4_3 = 1,
    R21_9 = 2,
    R16_10 = 3,
    StretchToWindow = 4,
}

impl From<u32> for AspectRatio {
    fn from(value: u32) -> Self {
        match value {
            0 => AspectRatio::Default,
            1 => AspectRatio::R4_3,
            2 => AspectRatio::R21_9,
            3 => AspectRatio::R16_10,
            4 => AspectRatio::StretchToWindow,
            _ => AspectRatio::Default,
        }
    }
}

/// Rectangle type for screen regions.
///
/// Corresponds to upstream `Common::Rectangle<u32>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rectangle {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Rectangle {
    pub fn new(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn get_width(&self) -> u32 {
        self.right - self.left
    }

    pub fn get_height(&self) -> u32 {
        self.bottom - self.top
    }

    pub fn translate_x(mut self, amount: u32) -> Self {
        self.left += amount;
        self.right += amount;
        self
    }

    pub fn translate_y(mut self, amount: u32) -> Self {
        self.top += amount;
        self.bottom += amount;
        self
    }
}

/// Describes the layout of the window framebuffer.
///
/// Corresponds to upstream `Layout::FramebufferLayout`.
#[derive(Debug, Clone)]
pub struct FramebufferLayout {
    pub width: u32,
    pub height: u32,
    pub screen: Rectangle,
    pub is_srgb: bool,
}

impl Default for FramebufferLayout {
    fn default() -> Self {
        Self {
            width: screen_undocked::WIDTH,
            height: screen_undocked::HEIGHT,
            screen: Rectangle::default(),
            is_srgb: false,
        }
    }
}

/// Finds the largest size subrectangle contained in window area
/// that is confined to the aspect ratio.
///
/// Corresponds to upstream anonymous `MaxRectangle`.
fn max_rectangle(window_area: &Rectangle, screen_aspect_ratio: f32) -> Rectangle {
    let scale =
        (window_area.get_width() as f32).min(window_area.get_height() as f32 / screen_aspect_ratio);
    Rectangle::new(
        0,
        0,
        (scale).round() as u32,
        (scale * screen_aspect_ratio).round() as u32,
    )
}

/// Factory method for constructing a default FramebufferLayout.
///
/// Corresponds to upstream `Layout::DefaultFrameLayout`.
pub fn default_frame_layout(width: u32, height: u32) -> FramebufferLayout {
    assert!(width > 0);
    assert!(height > 0);

    let mut res = FramebufferLayout {
        width,
        height,
        screen: Rectangle::default(),
        is_srgb: false,
    };

    let window_aspect_ratio = height as f32 / width as f32;
    let aspect = AspectRatio::from(*common::settings::values().aspect_ratio.get_value() as u32);
    let emulation_aspect_ratio = emulation_aspect_ratio(aspect, window_aspect_ratio);

    let screen_window_area = Rectangle::new(0, 0, width, height);
    let mut screen = max_rectangle(&screen_window_area, emulation_aspect_ratio);

    if window_aspect_ratio < emulation_aspect_ratio {
        screen = screen.translate_x((screen_window_area.get_width() - screen.get_width()) / 2);
    } else {
        screen = screen.translate_y((height - screen.get_height()) / 2);
    }

    res.screen = screen;
    res
}

/// Convenience method to get frame layout by resolution scale.
///
/// Corresponds to upstream `Layout::FrameLayoutFromResolutionScale`.
pub fn frame_layout_from_resolution_scale(res_scale: f32) -> FramebufferLayout {
    let is_docked = common::settings::is_docked_mode(&common::settings::values());
    let screen_width = if is_docked {
        screen_docked::WIDTH
    } else {
        screen_undocked::WIDTH
    };
    let screen_height = if is_docked {
        screen_docked::HEIGHT
    } else {
        screen_undocked::HEIGHT
    };

    let width = (screen_width as f32 * res_scale) as u32;
    let height = (screen_height as f32 * res_scale) as u32;

    default_frame_layout(width, height)
}

/// Convenience method to determine emulation aspect ratio.
///
/// Corresponds to upstream `Layout::EmulationAspectRatio`.
pub fn emulation_aspect_ratio(aspect: AspectRatio, window_aspect_ratio: f32) -> f32 {
    match aspect {
        AspectRatio::Default => screen_undocked::HEIGHT as f32 / screen_undocked::WIDTH as f32,
        AspectRatio::R4_3 => 3.0 / 4.0,
        AspectRatio::R21_9 => 9.0 / 21.0,
        AspectRatio::R16_10 => 10.0 / 16.0,
        AspectRatio::StretchToWindow => window_aspect_ratio,
    }
}
