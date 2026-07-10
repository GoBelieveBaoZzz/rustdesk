#!/usr/bin/env bash
cargo ndk --platform 21 --target aarch64-linux-android build --release --lib --features flutter,hwcodec
