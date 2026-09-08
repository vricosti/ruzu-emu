// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/bcat/news/
//! Upstream files:
//!   - service_creator.h / service_creator.cpp
//!   - news_service.h / news_service.cpp
//!   - news_database_service.h / news_database_service.cpp
//!   - news_data_service.h / news_data_service.cpp
//!   - newly_arrived_event_holder.h / newly_arrived_event_holder.cpp
//!   - overwrite_event_holder.h / overwrite_event_holder.cpp

pub mod newly_arrived_event_holder;
pub mod news_data_service;
pub mod news_database_service;
pub mod news_service;
pub mod news_storage;
pub mod overwrite_event_holder;
pub mod service_creator;
