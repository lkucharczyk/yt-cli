# yt-cli

## Installation
```sh
cargo install --git https://github.com/lkucharczyk/yt-cli
```

## Configuration
~/.config/yt-cli.cfg

```ini
[topic1]
channel1_name = channel1_id
channel2_name = channel2_id

[topic2]
channel3_id
```

## Usage
- `yt-cli` - shows latest videos from subscribed channels
- `yt-cli -t topic1` - shows latest videos from channels in the "topic1" topic
- `yt-cli -t topic1,topic2` - shows latest videos from channels in  "topic1" and "topic2" topics

## External dependencies:
- [jq](https://github.com/stedolan/jq)
- [xq](https://github.com/kislyuk/yq)
- [ueberzug](https://github.com/seebye/ueberzug)
- [youtube-dl](https://github.com/ytdl-org/youtube-dl)
- [mpv](https://github.com/mpv-player/mpv)
