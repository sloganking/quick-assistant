use clap::Parser;

use crate::{easy_rdev_key, SubCommands, VoiceEnum};

#[derive(Parser, Debug)]
#[command(version)]
pub struct Opt {
    /// The audio device to use for recording. Leaving this blank will use the default device.
    #[arg(long, default_value_t = String::from("default"))]
    pub device: String,

    /// Your OpenAI API key.
    #[arg(long)]
    pub api_key: Option<String>,

    /// The push-to-talk key used to activate the microphone.
    #[arg(long)]
    pub ptt_key: Option<easy_rdev_key::PTTKey>,

    /// The push-to-talk key as a special keycode.
    /// Use this if you want to use a key that is not supported by the `PTTKey` enum.
    /// You can find out what number to pass for your key by running the `ShowKeyPresses` subcommand.
    /// This option conflicts with `--ptt_key`.
    #[arg(long, conflicts_with("ptt_key"))]
    pub special_ptt_key: Option<u32>,

    /// How fast the AI speaks, with 1.0 as normal speed.
    /// The value must be between 0.5 (slowest) and 100.0 (fastest).
    #[arg(long, default_value_t = 1.0)]
    pub speech_speed: f32,

    /// Play a ticking sound while the AI is converting text to speech.
    #[arg(long)]
    pub tick: bool,

    /// Start with the AI voice muted.
    #[arg(long)]
    pub mute: bool,

    /// The voice that the AI will use to speak.
    /// Choose from a list of available voices to customize the output.
    #[arg(long)]
    pub ai_voice: Option<VoiceEnum>,

    /// The language model used to generate responses.
    /// Specify the name of the language model. For a list of available models, visit:
    /// https://platform.openai.com/docs/models/.
    /// Defaults to "gpt-4o".
    #[arg(long, default_value_t = String::from("gpt-4o"))]
    pub model: String,

    #[clap(subcommand)]
    pub subcommands: Option<SubCommands>,
}
