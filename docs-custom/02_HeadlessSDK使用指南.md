# Headless SDK 使用指南

> 完整 API 参数说明请参见 [03_API参考手册.md](./03_API参考手册.md)。

## 一、是什么

Headless SDK 是 RustDesk 的无 UI 控制端，通过 WebSocket 或管道（stdin/stdout）对外暴露 API，用 Python 脚本远程控制机器。

支持两种通信模式：

| 模式 | 适用场景 | 启动参数 |
|------|---------|---------|
| WebSocket | 调试、多客户端、远程管理 | `--port 9528` |
| 管道（stdin/stdout） | Python subprocess 拉起，同机部署 | `--pipe` |

```
管道模式: Python ──stdin/stdout──▶ headless_sdk ──RustDesk──▶ 远程机器
WebSocket: Python ──ws://localhost──▶ headless_sdk ──RustDesk──▶ 远程机器
```

## 二、编译

### 本地编译

```powershell
# Windows
cd D:\Projects\OtherProjects\rustdesk
. .\env.ps1
cargo build --bin headless_sdk            # Debug, ~30s
cargo build --bin headless_sdk --release  # Release

# Linux ARM64 (N1 盒子等)
cargo build --bin headless_sdk --release
```

### CI 下载

Actions → Flutter Build Lite → 勾选对应 headless 平台 → 下载产物。

## 三、WebSocket 模式

### 启动

```powershell
headless_sdk.exe                        # 默认 127.0.0.1:9528
headless_sdk.exe --port 8080            # 自定义端口
headless_sdk.exe --host 0.0.0.0 --port 9528  # 允许外部连接
```

### Python 示例

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

        await mouse.move_to(500, 300)
        await mouse.click()  # 在当前光标位置点击，不会用坐标移动
        await kb.chord("MetaLeft", ["2", "2"])
        img = await screen.capture()
        await bridge.disconnect()

asyncio.run(main())
```

## 四、管道模式

管道模式下 Python 作为父进程拉起 headless，通过 stdin/stdout 通信，无需网络端口。

### 启动

```powershell
# Windows
headless_sdk.exe --pipe

# Linux ARM64
./headless_sdk --pipe
```

### Python 示例

```python
import subprocess, json

proc = subprocess.Popen(
    ['./headless_sdk', '--pipe'],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)

def send_cmd(cmd: dict) -> dict:
    """发送一条命令，返回 JSON 响应。含截图时自动读取二进制帧。"""
    line = json.dumps(cmd) + '\n'
    proc.stdin.write(line.encode())
    proc.stdin.flush()

    resp_line = proc.stdout.readline()
    resp = json.loads(resp_line)

    if resp.get('has_frame'):
        w = resp['w']
        h = resp['h']
        stride = resp['stride']
        proc.stdout.read(4)  # skip magic bytes "RSDK"
        raw = proc.stdout.read(stride * h)
        resp['_raw'] = raw  # 原始像素数据

    return resp

# 使用
send_cmd({"id": 1, "cmd": "connect", "peer_id": "123456789", "password": "xxx"})
send_cmd({"id": 2, "cmd": "mouse", "action": "move_to", "x": 500, "y": 300})
send_cmd({"id": 3, "cmd": "mouse", "action": "click", "button": "left"})
result = send_cmd({"id": 4, "cmd": "screenshot"})
# result['_raw'] 是 ABGR 格式的原始像素

proc.terminate()
```

### 管道模式注意

- 每条命令一行 JSON，以 `\n` 结尾
- 响应先写二进制帧（如果有），再写 JSON + `\n`
- 错误事件输出到 stderr，不污染 stdout
- 退出前输出断开事件：`{"event":"disconnected","reason":"pipe closed"}`
- 断开连接或 EOF 时自动清理远程会话

## 五、协议格式

两种模式使用相同的 JSON 命令/响应格式：

```
命令 (JSON Text):
→ {"id":1, "cmd":"connect", "peer_id":"123456789", "password":"xxx"}
→ {"id":2, "cmd":"disconnect"}
→ {"id":3, "cmd":"screenshot"}
→ {"id":4, "cmd":"mouse", "action":"move_to", "x":500, "y":300}
→ {"id":5, "cmd":"mouse", "action":"click", "button":"left"}
→ {"id":6, "cmd":"keyboard", "action":"key_click", "key":"Enter"}
→ {"id":7, "cmd":"key_sequence", "keys_seq":[...]}
→ {"id":8, "cmd":"status"}
→ {"id":9, "cmd":"ping"}

响应 (JSON Text):
← {"id":1, "ok":true, "state":"connecting"}
← {"id":3, "ok":true, "has_frame":true, "w":1920, "h":1080, "format":"abgr", "stride":7680}
← (后跟二进制帧: [magic:u32 LE "RSDK"][w:u32 LE][h:u32 LE][fmt:u32 LE][stride:u32 LE][pixels])
← {"id":8, "ok":true, "connected":true, "has_session":true}

事件 (WebSocket & 管道):
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
