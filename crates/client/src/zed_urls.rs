//! Contains helper functions for constructing URLs to various pages.
//!
//! Zerminal does not ship a managed billing/subscription service, so the
//! account/trial/upgrade/blog URLs that Zed surfaces in its UI all point
//! at the Zerminal project README. If a managed service is introduced
//! later, update these to point at it.

use gpui::App;

const ZERMINAL_README: &str = "https://github.com/elleryfamilia/zerminal#readme";

pub fn account_url(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn start_trial_url(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn upgrade_to_zed_pro_url(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn terms_of_service(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn ai_privacy_and_security(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn edit_prediction_docs(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn acp_registry_blog(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn parallel_agents_blog(_cx: &App) -> String {
    ZERMINAL_README.to_string()
}

pub fn shared_agent_thread_url(session_id: &str) -> String {
    format!("zerminal://agent/shared/{}", session_id)
}
