# API 参考手册

> Headless SDK 完整 API 参考。使用指南见 [HeadlessSDK使用指南.md](HeadlessSDK使用指南.md)。

## WebSocket 协议层

`headless_sdk.exe` 在 `ws://127.0.0.1:9528/ws` 提供 WebSocket 服务。

### 命令格式

请求 JSON: `{"id": 1, "cmd": "connect", "...": "..."}`
响应 JSON: `{"id": 1, "ok": true}`

### connect — 连接

```json
{"cmd": "connect", "peer_id": "123456789", "password": "xxx"}
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| peer_id | str | ✅ | 远程机器 RustDesk ID |
| password | str | ❌ | 连接密码 |

### disconnect — 断开

```json
{"cmd": "disconnect"}
```

### get_id — 获取本机 ID

```json
{"cmd": "get_id"}
// → {"id":3, "ok":true, "peer_id":"37513141"}
```

### set_id — 修改本机 ID

新 ID 在下次重启后生效。

```json
{"cmd": "set_id", "peer_id": "MyCustomId"}
{"cmd": "set_id", "peer_id": "MyCustomId", "check_server": true}
```

### screenshot — 截图

```json
{"cmd": "screenshot"}
```

响应: `{"ok":true, "has_frame":true, "w":1920, "h":1080, "format":"abgr", "stride":7680, "fmt_code":0}`

### mouse — 鼠标

```json
{"cmd": "mouse", "action": "click", "x": 500, "y": 300, "button": "left"}
```

| action | 说明 | 需要 x/y |
|--------|------|----------|
| move_to | 移动光标到绝对坐标 | ✅ |
| move_relative | 相对移动 (dx,dy) | ✅ |
| click | 当前位置点击 | ❌ |
| down / up | 按下/释放 | ❌ |
| scroll | 滚轮 | dy |

button: `"left"` / `"right"` / `"middle"` / `"back"` / `"forward"`

### keyboard — 键盘

```json
{"cmd": "keyboard", "action": "key_click", "key": "Enter"}
```

action: `key_down` / `key_up` / `key_click`

### key_sequence — 按键序列（推荐）

Map 模式，直接发扫描码，避免修饰键冲突。

```json
{
  "cmd": "key_sequence",
  "keys_seq": [
    {"action": "key_down", "key": "MetaLeft", "delay_ms": 50},
    {"action": "key_click", "key": "2"},
    {"action": "wait", "delay_ms": 200},
    {"action": "key_click", "key": "2"},
    {"action": "key_up", "key": "MetaLeft"}
  ]
}
```

| action | 说明 | 需要 key |
|--------|------|----------|
| key_down | 按下 | ✅ |
| key_up | 释放 | ✅ |
| key_click | 按下→50ms→释放 | ✅ |
| wait | 等待 N ms | ❌ |

### 画质/帧率/分辨率

```json
{"cmd": "set_quality", "quality": "best"}
{"cmd": "set_quality", "quality": "custom", "value": 80}
{"cmd": "set_fps", "fps": 5}
{"cmd": "set_resolution", "display": 0, "width": 1280, "height": 720}
```

### 事件推送

```json
{"event": "connected"}
{"event": "disconnected", "reason": "closed"}
```

### 截图二进制帧

```
字节 0-3:   width  (u32 LE)
字节 4-7:   height (u32 LE)
字节 8-11:  fmt    (0=ABGR, 1=ARGB, 2=RGB)
字节 12-15: stride
字节 16+:   pixels
```
