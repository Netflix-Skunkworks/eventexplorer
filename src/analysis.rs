// Copyright 2026 Netflix, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Higher-level analytics over parsed `.elog` data.
//!
//! Everything here consumes the parser types from [`crate::elog_parser`] and
//! produces the session/rate/limiter/goodput/cpu/flame summaries that the GUI
//! and CLI render. The dependency is one-directional: the parser never depends
//! on this module.

mod session;
pub use session::*;
mod rate;
pub use rate::*;
mod limiter;
pub use limiter::*;
mod goodput;
pub use goodput::*;
mod cpu;
pub use cpu::*;
mod flame;
pub use flame::*;
mod functions;
pub use functions::*;
mod samples;
pub use samples::*;
mod nginx;
pub use nginx::*;
mod memory;
pub use memory::*;
mod segments;
pub use segments::*;
