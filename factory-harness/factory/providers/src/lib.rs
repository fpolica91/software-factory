mod profiles;

#[cfg(feature = "adapter")]
mod anthropic;
#[cfg(feature = "adapter")]
mod chat;
#[cfg(feature = "adapter")]
mod config;
#[cfg(feature = "adapter")]
mod response_stream;
#[cfg(feature = "adapter")]
mod responses;
#[cfg(feature = "adapter")]
mod server;
#[cfg(feature = "adapter")]
mod sse;
#[cfg(feature = "adapter")]
mod tools;

#[cfg(feature = "adapter")]
pub use config::AdapterConfig;
pub use profiles::AdapterKind;
pub use profiles::BaseUrlChoice;
pub use profiles::CodexProviderSelection;
pub use profiles::GENERATED_MODEL_CATALOG_PATH;
pub use profiles::ProviderProfile;
pub use profiles::provider_profile;
pub use profiles::provider_profiles;
#[cfg(feature = "adapter")]
pub use server::ProviderAdapter;

#[cfg(test)]
mod tests;
