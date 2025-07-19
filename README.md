# Quick Assistant

```
   ____        _      __      ___              _      __              __
  / __ \__  __(_)____/ /__   /   |  __________(_)____/ /_____ _____  / /_
 / / / / / / / / ___/ //_/  / /| | / ___/ ___/ / ___/ __/ __ `/ __ \/ __/
/ /_/ / /_/ / / /__/ ,<    / ___ |(__  |__  ) (__  ) /_/ /_/ / / / / /_
\___\_\__,_/_/\___/_/|_|  /_/  |_/____/____/_/____/\__/\__,_/_/ /_/\__/
```

[![Build Status](https://github.com/sloganking/quick-assistant/actions/workflows/rust.yml/badge.svg)](https://github.com/sloganking/quick-assistant/actions/workflows/rust.yml)

A push-to-talk AI voice assistant for your desktop.

quick-assistant is a CLI program for your desktop. It lets you hold a key and talk to a GPT-4 Turbo–powered assistant anytime. Responses come in text and voice, so conversations feel natural. The AI can be interrupted mid-sentence when you need to redirect it.

https://github.com/sloganking/quick-assistant/assets/16965931/a0c7469a-2c64-46e5-9ee9-dd9f9d56ea95

## Features

- 🌞 **Screen brightness** control
- 🔊 **System volume** adjustment (Windows only)
- ⏯️ **Media playback** commands
- 🚀 **Launch applications** from voice
- 📑 **Display log files** for troubleshooting
- 🖥️ **Get system info** on demand
- 🗑️ **List and kill processes** by voice
- 🌐 **Run internet speed tests**
- 📋 **Set the clipboard** contents
- 📋 **Get the clipboard** contents
- 🔳 **Copy text as a QR code image** to the clipboard
- ⏱️ **Timers** with alarm sounds
- 🎙️ **Change voice** or speaking speed on the fly
- 🔕 **Mute/unmute** the voice output
- 💸 **Open OpenAI billing** page in the browser

## Setup

> **Note**: Run the setup script first to install system dependencies.
>
> ```bash
> ./setup.sh
> ```

## Usage

Start the assistant with cargo:

```bash
cargo run --release
```

Use `--help` for more options.
