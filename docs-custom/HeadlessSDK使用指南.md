# Headless SDK 使用指南

> 完整 API 参数说明请参见 [API参考手册.md](API参考手册.md)。

## 一、是什么

Headless SDK 是 RustDesk 的无 UI 控制端，通过 WebSocket 对外暴露 API，用 Python 脚本远程控制机器。

```
Python 脚本 ──WebSocket──▶ headless_sdk.exe ──RustDesk 协议──▶ 远程机器
```

## 二、编译

```powershell
cd D:\Projects\rustdesk
. .\env.ps1
cargo build --bin headless_sdk            # Debug, ~30s
cargo build --release --bin headless_sdk   # Release, ~4min
```

## 三、启动

```powershell
.\target\debug\headless_sdk.exe
# 或指定端口: headless_sdk.exe --port 9528
```

日志: `%APPDATA%\RustDesk\log\headless_sdk\`

## 四、Python 示例

```python
import asyncio
from bridge import Bridge
from mouse import Mouse
from keyboard import Keyboard
from screen import Screen

async def main():
    async with Bridge("ws://127.0.0.1:9528/ws") as bridge:
        await bridge.connect("37513141", "password")
        mouse = Mouse(bridge)
        kb = Keyboard(bridge)
        screen = Screen(bridge)

        await mouse.click(500, 300)
        await kb.chord("MetaLeft", ["2", "2"])
        img = await screen.capture()  # numpy (H,W,3) RGB
        await bridge.disconnect()

asyncio.run(main())
```

## 五、WebSocket 协议

```
命令 (JSON Text):
→ {"id":1, "cmd":"connect", "peer_id":"123456789", "password":"xxx"}
→ {"id":2, "cmd":"screenshot"}
→ {"id":3, "cmd":"mouse", "action":"click", "x":500, "y":300, "button":"left"}
→ {"id":4, "cmd":"keyboard", "action":"key_click", "key":"Enter"}
→ {"id":5, "cmd":"key_sequence", "keys_seq":[...]}

响应 (JSON Text):
← {"id":1, "ok":true}
← {"id":2, "ok":true, "has_frame":true, "w":1920, "h":1080, "format":"abgr", "stride":7680}
← (后跟二进制帧: [w:u32 LE][h:u32 LE][fmt:u32 LE][stride:u32 LE][pixels])

事件 (服务端推送):
← {"event":"connected"}
← {"event":"disconnected", "reason":"closed"}
```

## 六、键名速查

| 类别 | 键名 |
|------|------|
| 字母 | `"a"` ~ `"z"` |
| 数字 | `"0"` ~ `"9"` |
| 方向键 | `"ArrowUp"`, `"ArrowDown"`, `"ArrowLeft"`, `"ArrowRight"` |
| 修饰键 | `"ControlLeft"`, `"ShiftLeft"`, `"Alt"`, `"MetaLeft"` |
| 功能键 | `"F1"` ~ `"F12"` |
| 特殊 | `"Enter"`, `"Space"`, `"Tab"`, `"Escape"`, `"Backspace"`, `"Delete"` |

## 七、踩坑记录

| 问题 | 原因 | 解决 |
|------|------|------|
| 截图收不到 | WebSocket max_size=1MB | max_size=50MB |
| 颜色偏蓝紫 | ARGB 内存布局问题 | `[:,:,[2,1,0]]` 还原 RGB |
| 组合键不生效 | Legacy sync_modifiers | 用 key_sequence (Map 模式) |
| 单击坐标不对 | MOUSE_DOWN 不处理坐标 | click 先 move 再 down |
