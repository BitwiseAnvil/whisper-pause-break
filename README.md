# Whisper Pause/Break

**Hold a key, talk, let go, and your words appear.** Whisper Pause/Break is
push-to-talk dictation for Windows. Hold **Pause/Break**, speak, and release:
Whisper transcribes on your own PC and types the text at your cursor, in any
app. Nothing is sent to the cloud, and nothing is recorded unless you're holding
the key, so the people around you can talk freely.

Built by **[Bitwise Anvil LLC](https://bitwiseanvil.com/)**. If you like what
you see, [let's talk](#work-with-bitwise-anvil) about what we can build for you.

Pair it with **[Local Qwen Voice](https://github.com/BitwiseAnvil/local-qwen-voice)**,
which gives Claude Code and Codex a local spoken voice, for hands-free
conversations with your AI coding tools.

## What it does

- **Push-to-talk, not always listening.** Only audio captured while you hold the
  key is transcribed. Short taps and silence are ignored.
- **Fast and local.** With the default `base.en` model, an 11-second clip
  transcribes in about **62 ms on an RTX 5090** and about half a second on CPU.
  Without a usable NVIDIA GPU it falls back to the CPU automatically.
- **Types anywhere.** Text goes in through standard Windows input at the cursor
  of whatever app is focused, with a trailing space so consecutive takes don't
  run together. It never presses Enter or Tab, and never touches your clipboard.
- **Changed your mind?** Press **Escape** while still holding the key to throw
  the take away.
- **Audible cues.** Short ticks confirm start, stop, insertion and cancel, so
  you don't have to watch the screen.
- **Set and forget.** Installs per user with no administrator rights, starts at
  login, and lives in the system tray.
- **Private by design.** Audio stays in memory, and neither audio nor transcripts
  are written to logs. The only network access is the one-time, checksum-verified
  model download.

Written entirely in Rust for Windows 10/11 x64. The `whisper-rs` bindings build
the upstream [whisper.cpp](https://github.com/ggml-org/whisper.cpp) library.

## Install with Claude Code or Codex

Let your AI assistant do the setup. It can clone the repo, build the app, walk
you through the keyboard check, download the model and install it.

**Install these yourself first.** They need administrator rights or your own
sign-in:

- Windows 10 or 11 x64 with an AVX2-capable CPU, a microphone, and a keyboard
  whose Pause/Break key sends a real release event (the assistant will check; see
  [Keyboard compatibility](#keyboard-compatibility--check-once)).
- [Git](https://git-scm.com/) and [Rust](https://rustup.rs/) stable 1.89 or newer
  with the MSVC toolchain.
- [Visual Studio or Build Tools](https://visualstudio.microsoft.com/downloads/)
  with the **Desktop development with C++** workload, including the Windows SDK
  and CMake tools.
- [LLVM](https://github.com/llvm/llvm-project/releases), which provides `libclang.dll`.
- Optional, for GPU speed: an NVIDIA GPU, a current driver and the
  [CUDA Toolkit](https://developer.nvidia.com/cuda-downloads). Developed with CUDA 13.
  Without them the app runs on the CPU.
- [Claude Code](https://claude.com/claude-code) or
  [Codex CLI](https://developers.openai.com/codex/cli), signed in.

**Then** open a terminal in an empty folder, start `claude` or `codex`, and paste:

```text
Install Whisper Pause/Break from https://github.com/BitwiseAnvil/whisper-pause-break on this PC.
Clone it into this folder, read its README and follow "Build" and "Install once". First check
the prerequisites and tell me what's missing instead of installing system software yourself.
Build the CPU host. If I have an NVIDIA GPU and the CUDA Toolkit, also build the CUDA worker.
Before installing, run key-test and ask me to hold Pause/Break for two seconds and release.
If Down and Up aren't about two seconds apart, stop and explain the README's options.
Then download the model, install the app and confirm the log shows it is ready.
```

The assistant will ask before running commands; approve them as it goes. The
first build compiles whisper.cpp and takes a few minutes. **Codex users:**
Codex's sandbox blocks network access and changes outside the folder by default,
and the build downloads crates and the installer writes to your profile.
Approve Codex's requests to run those steps outside the sandbox. When it's
finished, look for **Ready** in the tray menu, then hold Pause/Break and talk.

## Keyboard compatibility — check once

Pause is unusual: some keyboards/drivers send no real release event, or synthesize a release immediately at key-down. **True hold-to-talk requires a real release event.** This app does not guess release from silence, a timer, or `GetAsyncKeyState`, and does not silently change the interaction to a toggle.

Before installing, run `key-test`, hold Pause for two seconds, and release. `Down` and `Up` should be about two seconds apart:

```powershell
.\dist\whisper-pause-break.exe key-test
```

The test runs for 30 seconds without opening the microphone. It logs only the configured hotkey and Escape. Stop a running app first with `stop`. If `Up` is missing or immediate, map the **physical Pause key to F13 in keyboard firmware/vendor software that emits real make/break events**, and set `hotkey = "F13"`. Alternatively choose ScrollLock or F14–F24. A Windows software remap cannot recover a hardware release event that was never sent; injected keyboard events are deliberately ignored. The default remains Pause.

## Build

Install these build dependencies:

- Rust stable **1.89 or newer**, `x86_64-pc-windows-msvc`.
- Visual Studio / Build Tools with **Desktop development with C++**, Windows SDK, and CMake tools including Ninja. This compiler builds dependencies only; the application's code is Rust.
- LLVM with `LIBCLANG_PATH` pointing to its `bin` directory. Windows bindings must be generated; `WHISPER_DONT_GENERATE_BINDINGS` is not supported with the Linux bindings bundled in whisper-rs-sys 0.15.
- The Microsoft Visual C++ x64 Redistributable is needed on machines that do not already have the MSVC runtime installed.
- For CUDA: an NVIDIA driver and CUDA Toolkit supported by your GPU and MSVC toolset. This project's build helper prefers MSVC 14.44 when installed alongside VS 2026, for compatibility with CUDA 13.

The helper also recognizes `.tools/libclang/clang/native/libclang.dll` if you choose to unpack libclang there. Python is not required to build or run the application.

```powershell
# CPU host (required, even when using CUDA)
.\scripts\build.ps1 -Package

# Optional accelerated worker + CUDA runtime DLLs
.\scripts\build.ps1 -Cuda -Package

# Automated tests
.\scripts\build.ps1 -Test
.\scripts\build.ps1 -Lint
```

CUDA kernels default to the build machine's GPU architecture (`native`). To distribute across different GPUs, pass e.g. `-CudaArchitectures '86;89;120'` for the architectures your installed toolkit supports. The CPU build assumes an AVX2-capable x64 CPU. Models and CUDA runtime files are not committed to git. Review NVIDIA's redistributable license before distributing its DLLs.

The `dist` directory contains `whisper-pause-break.exe` (CPU host), optional `whisper-pause-break-cuda.exe`, and its CUDA runtime DLLs. The host has **no CUDA DLL dependency**. A missing CUDA worker, driver, GPU, or runtime falls back to the CPU host. The optional worker must be next to the host executable.

## Install once

From PowerShell:

```powershell
# The only network operation in normal setup: download official model weights.
# The download is checked against a pinned SHA-256 and file size.
.\dist\whisper-pause-break.exe download-model

# Copy the application into your profile, register login startup, and start now.
.\dist\whisper-pause-break.exe install
```

The default data directory is `%LOCALAPPDATA%\WhisperPauseBreak`; executables go in its `bin` subdirectory. Installation creates a quoted command under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\WhisperPauseBreak`. No administrator permission, service, scheduled task, console window, or manual launch is required on subsequent logins. Windows may delay Run entries briefly after login. Wait for **Ready · CUDA** or **Ready · CPU** in the tray menu before dictating.

Already have an appropriate GGML Whisper model? Use `install --model C:\path\ggml-base.en.bin`. Keep `language = "en"` for English-only models. Use a multilingual model with another language code or `"auto"`. The model is loaded once and kept in memory. `base.en` is the default latency/accuracy compromise; choose a larger model by editing the config if desired.

For a larger CUDA model, [large-v3-turbo](https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-large-v3-turbo.bin) has 809 million parameters and a 1.62 GB GGML file. Install a downloaded copy with `install --model C:\path\ggml-large-v3-turbo.bin`; keep `language = "en"` for English dictation. It uses more GPU memory than base.en. The verified SHA-256 for this file is `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69`. The download and model switch happen once; all subsequent transcription remains local.

`install --no-start` enables startup without launching immediately. To run portably, use `--data-dir C:\path\data run`; the same global option applies to all commands. Installation and the one-time model download are explicit, never performed during background startup.

## Interaction

| Event | Behavior | Cue |
| --- | --- | --- |
| Press and hold Pause | Start capturing the default microphone | 35 ms rising tick |
| Release Pause | Stop capture and transcribe the whole take | 45 ms falling tick |
| Finished text accepted by Windows input | Type the entire transcript at the current cursor | 80 ms double tick |
| Escape while held | Discard the take; release before starting again | 85 ms low falling double tick |

The hotkey and cancellation Escape are consumed so they do not act on the focused application. Escape plays the cancellation cue once when it discards an active recording; subsequent Escape presses and releasing Pause do not play another cue or submit audio. Ordinary Escape passes through silently. Key-repeat cannot restart a cancelled recording. A second press while transcribing is ignored; wait for completion before the next take. No audio is queued for later insertion. Silence, very short taps, disconnected microphones, and recordings over the configured duration are discarded. Locking/suspending Windows discards active capture and suppresses pending results.

**Optional: block Win+H.** Off by default. Set `block_win_h = true` in `config.toml` and restart to stop Windows voice typing from opening with Win+H while the app runs. When enabled, either Windows key works; held H repeats and its release are consumed. Plain H, other Windows shortcuts, and H shortcuts with extra Ctrl/Alt/Shift modifiers pass through. Quitting the app restores Win+H automatically. No Windows setting or separate remapping utility is required; diagnostic CLI commands do not enable the block.

Right/left-click the tray icon to see status, open configuration/logs, or quit. Only one instance can run in a Windows session. Configuration changes require a restart. Audio is kept in memory only; neither audio nor transcript contents are written to logs. The microphone stream stays open to minimize press latency, but samples outside an active recording are discarded immediately. Windows' microphone indicator may therefore stay on while idle.

The default microphone is checked every two seconds while idle and rebuilt after a device change/failure. Use headphones to prevent the short start cue from entering a sensitive microphone. A simple energy gate rejects silence and isolated clicks; Whisper can still make recognition mistakes, especially in noise.

Text is sent through `SendInput` using UTF-16 (including surrogate pairs). Control characters/newlines are normalized to spaces so dictation cannot press Enter or Tab. A trailing space is appended after each take so consecutive dictations do not run together. The completion cue follows successful submission of **all** input events to Windows; arbitrary applications do not expose a universal acknowledgement that they displayed the text. Most standard text fields work. Elevated applications can block a normal user's synthetic input through Windows UIPI, and some games, terminals, or custom editors may reject Unicode input. Insertion is not retried after partial failure because that could duplicate text. Secure desktops/UAC prompts are outside the supported interaction.

## Diagnostics

```powershell
.\dist\whisper-pause-break.exe doctor
.\dist\whisper-pause-break.exe cue-test
.\dist\whisper-pause-break.exe key-test
.\dist\whisper-pause-break.exe transcribe C:\audio\sample.wav --output C:\audio\transcript.txt
.\dist\whisper-pause-break.exe transcribe C:\audio\sample.wav --cpu
.\dist\whisper-pause-break.exe stop
.\dist\whisper-pause-break.exe uninstall
```

`transcribe` uses the same persistent worker and resampling pipeline, but never inserts text into another application. `app.log` records backend choice, errors, character counts, and inference/release-to-insertion timings. `cuda.log` / `cpu.log` contain native model/backend diagnostics. Logs rotate/truncate on startup when larger than 2 MB. `uninstall` stops the app and removes automatic startup, retaining the model, config and files so reinstalling does not require another download.

Windows GUI executables do not always make PowerShell wait. To synchronously wait for a diagnostic command, pipe to `Out-Host` or use `Start-Process -Wait -NoNewWindow`. Background `run` should normally be launched by the installer or Windows login.

## Design and verification

- A minimal `WH_KEYBOARD_LL` hook runs on the tray message loop. It sends control events and, when `block_win_h` is enabled, filters Win+H; inference never blocks the hook. A dummy key pair masks Start-menu activation after a blocked Win+H without changing held modifiers.
- WASAPI audio via CPAL keeps capture/playback streams warm. Multichannel capture is downmixed and FFT-resampled to 16 kHz with anti-aliasing and delay compensation.
- The pure state machine handles hold, release, repeat, busy presses, cancellation, and time limits.
- A local anonymous pipe carries bounded PCM frames to one persistent Whisper worker. JSON responses are length-delimited and bounded. No network sockets or temporary WAV files are used.
- A Windows job object kills the child when the host exits. CUDA initialization failures, native aborts, disconnects, and inference timeouts trigger CPU fallback. Failed GPU inference is retried once on CPU, before any text is inserted.
- Whisper uses greedy decoding, no prior take context, no realtime text callbacks, no temperature retries, and optional flash attention on CUDA. It performs a warm-up at startup.
- Unit tests cover cancellation/repeat/order, Win+H release order and preservation of other H shortcuts, audio length/gain and anti-aliasing, silence/cue rejection, bounded IPC, Unicode event pairs, normalization, configuration validation, and quoted startup commands.

Verified on the development PC (2026-10-04): 24 unit tests, Clippy with warnings denied, CPU and CUDA release builds, identical reference-WAV transcripts on both backends, automatic CPU fallback with CUDA DLLs unavailable, and background startup/audio/shutdown using `scripts/smoke.ps1`. With base.en, the 11-second upstream `jfk.wav` sample took approximately **62 ms on an RTX 5090** and **547 ms on CPU** for warm inference. These timings exclude startup, capture, and text insertion and are not a universal latency guarantee. Cue playback queues complete cues in order, including when inference finishes before the stop cue ends. Recoverable WASAPI overruns do not trigger stream reconnect loops.

Physical key release behavior, perceived cue volume, microphone quality, and compatibility with your target editors require a desktop acceptance check. Passing automated tests does not prove a keyboard emits Pause releases.

Upstream references: [whisper-rs](https://docs.rs/whisper-rs/0.16.0/whisper_rs/), [whisper.cpp](https://github.com/ggml-org/whisper.cpp), [CPAL](https://docs.rs/cpal/0.18.2/cpal/), [Microsoft keyboard hooks](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc), [Microsoft SendInput and UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput), [official GGML model](https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-base.en.bin).

## Built on

| Project | License |
| --- | --- |
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) and its [GGML Whisper models](https://huggingface.co/ggerganov/whisper.cpp) | MIT |
| [OpenAI Whisper](https://github.com/openai/whisper) model weights | MIT |
| [whisper-rs](https://github.com/tazz4843/whisper-rs) Rust bindings | Unlicense |
| [CPAL](https://github.com/RustAudio/cpal) audio and the other crates in `Cargo.toml` | See each crate |

Model weights are downloaded on request and are not part of this repository. CUDA
runtime DLLs are covered by NVIDIA's own license; review it before redistributing
a CUDA build.

## About this project

Whisper Pause/Break was designed and written with substantial help from AI tools,
including Claude Code and Codex, and is tested on the author's own Windows and
RTX 5090 setup. It is shared as a working example and a useful tool, provided
"as is" without warranty. Issues and pull requests are welcome.

## Work with Bitwise Anvil

Whisper Pause/Break is built by **[Steven Cheatham](https://stevencheatham.com/)**
of **[Bitwise Anvil LLC](https://bitwiseanvil.com/)**: *Precision software,
forged at AI speed.*

This project is a working example of what we do: dependable, thoroughly
tested systems, engineered with AI and built to last. Bitwise Anvil offers
systems architecture, advisory, custom development, custom web development,
AI agents and system audits.

- **Work with Bitwise Anvil:** [bitwiseanvil.com](https://bitwiseanvil.com/) ·
  [contact@bitwiseanvil.com](mailto:contact@bitwiseanvil.com)
- **Connect with Steven:** [LinkedIn](https://www.linkedin.com/in/stevencheatham/) ·
  [X](https://x.com/StevenCheatham) ·
  [steven@stevencheatham.com](mailto:steven@stevencheatham.com)

If this project helped you, a ⭐ on GitHub helps others find it.

## License

[MIT](LICENSE).
