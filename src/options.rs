use clap::Parser;

use crate::{easy_rdev_key, SubCommands, VoiceEnum};

#[derive(Parser, Debug)]
#[command(version)]
pub struct Opt {
    /// The audio device to use for recording. Leaving this blank will use the default device.
    #[arg(long, default_value_t = String::from("default"))]
    pub device: String,

    /// Your OpenAI API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// The push to talk key
    #[arg(long)]
    pub ptt_key: Option<easy_rdev_key::PTTKey>,

    /// The push to talk key.
    /// Use this if you want to use a key that is not supported by the PTTKey enum.
    #[arg(long, conflicts_with("ptt_key"))]
    pub special_ptt_key: Option<u32>,

    /// How fast the AI speaks. 1.0 is normal speed.
    /// 0.5 is minimum. 100.0 is maximum.
    #[arg(long, default_value_t = 1.0)]
    pub speech_speed: f32,

    /// The voice that the AI will use to speak.
    #[arg(long)]
    pub ai_voice: Option<VoiceEnum>,

    /// The language model used to generate responses.
    ///
    /// You can find the names of more models here.
    /// https://platform.openai.com/docs/models/
    #[arg(long, default_value_t = String::from("gpt-4o"))]
    pub model: String,

    #[clap(subcommand)]
    pub subcommands: Option<SubCommands>,
}
