use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
pub enum RealTimeEvents {
  Ping,
  // ChatStatus { channel_id: String, status: String },
  PushToken { token: String }
}