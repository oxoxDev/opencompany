use super::*;
use std::sync::Mutex as StdMutex;

use crate::ports::runs::RunStatus;
use crate::ports::types::{CompanyRecord, CompanySummary, LedgerEntry};

#[path = "orchestrator_tests_support1.rs"]
mod support1;
#[path = "orchestrator_tests_support2.rs"]
mod support2;
use support1::*;
use support2::*;

#[path = "orchestrator_tests_general_channel.rs"]
mod tests_general_channel;
#[path = "orchestrator_tests_part1.rs"]
mod tests_part1;
#[path = "orchestrator_tests_part10.rs"]
mod tests_part10;
#[path = "orchestrator_tests_part11.rs"]
mod tests_part11;
#[path = "orchestrator_tests_part2.rs"]
mod tests_part2;
#[path = "orchestrator_tests_part3.rs"]
mod tests_part3;
#[path = "orchestrator_tests_part4.rs"]
mod tests_part4;
#[path = "orchestrator_tests_part5.rs"]
mod tests_part5;
#[path = "orchestrator_tests_part6.rs"]
mod tests_part6;
#[path = "orchestrator_tests_part7.rs"]
mod tests_part7;
#[path = "orchestrator_tests_part8.rs"]
mod tests_part8;
#[path = "orchestrator_tests_part9.rs"]
mod tests_part9;
