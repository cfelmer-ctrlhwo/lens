//! Lens adapters — convert AI tool session logs into agent-activity.v1 events.
//!
//! V1: claude_code only. Codex, Grok, Paperclip, others land V1.x+.
#![allow(dead_code)]  // skeleton — many items used by Week 1 work only

pub mod claude_code;
