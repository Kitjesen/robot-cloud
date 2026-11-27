#!/usr/bin/env python3
"""Admin-only robot remote operations (service control, live logs, SFTP file manager).

Connects either through the cloud 443-WSS reverse tunnel on SWAS (127.0.0.1:22001-22999)
or directly over office LAN via Paramiko SSH/SFTP. Strictly internal to the local console.
"""

from __future__ import annotations

import contextlib
import json
import os
import posixpath
import re
import shlex
import stat
import time
from pathlib import Path
from typing import Any, Iterator

ALLOWED_SERVICES = (
    "doso-brainstem.service",
    "doso-ota.service",
    "doso-cloud-link.service",
    "doso-ops-tunnel.service",
)
ALLOWED_SERVICE_ACTIONS = ("restart", "start", "stop")

PROTECTED_DELETE_PATHS = frozenset(
    {
        "/",
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/etc/doso",
        "/home",
        "/home/sunrise",
        "/lib",
        "/lib64",
        "/opt",
        "/opt/doso",
        "/opt/doso/current",
        "/opt/doso/data",
        "/opt/doso/components",
        "/proc",
        "/root",
        "/run",
        "/sbin",
        "/sys",
        "/tmp",
        "/usr",
        "/var",
        "/var/lib",
        "/var/lib/doso-ota",
    }
)

MAX_TEXT_EDIT_BYTES = 512 * 1024
MAX_DOWNLOAD_BYTES = 128 * 1024 * 1024

QUICK_DIRS = (
    {"label": "主目录 /opt/doso", "path": "/opt/doso"},
    {"label": "数据目录", "path": "/opt/doso/data"},
    {"label": "动作库", "path": "/opt/doso/data/gestures"},
    {"label": "策略包", "path": "/opt/doso/data/models/packages"},
    {"label": "系统配置", "path": "/etc/doso"},
    {"label": "OTA 状态", "path": "/var/lib/doso-ota"},
    {"label": "临时目录", "path": "/tmp"},
)


class RobotOpsError(ValueError):
    """Actionable robot remote operations error."""


def normalize_remote_path(raw: str) -> str:
    text = (raw or "").strip()
    if not text or "\0" in text:
        raise RobotOpsError("路径无效")
    if not text.startswith("/"):
        raise RobotOpsError("必须使用以 / 开头的绝对路径")
    norm = posixpath.normpath(text)
    if not norm.startswith("/"):
        raise RobotOpsError("路径无效")
    return norm


def validate_service(unit: str) -> str:
    cleaned = (unit or "").strip()
    if cleaned not in ALLOWED_SERVICES:
        raise RobotOpsError(f"不在允许控制的服务白名单内: {cleaned}")
    return cleaned


def validate_service_action(action: str) -> str:
    cleaned = (action or "").strip()
    if cleaned not in ALLOWED_SERVICE_ACTIONS:
        raise RobotOpsError(f"不支持的服务操作: {cleaned}")
    return cleaned


def _require_paramiko() -> Any:
    try:
        import paramiko

        return paramiko
    except ImportError as exc:
        raise RobotOpsError("本机未安装 paramiko，无法建立加密运维通道") from exc


def _resolve_jump_key(cfg: dict[str, str]) -> Path:
    configured = (cfg.get("DOSO_OPS_JUMP_KEY") or "").strip()
    if configured:
        p = Path(configured).expanduser()
        if p.is_file():
            return p
        raise RobotOpsError(f"跳板私钥文件不存在: {p}")
    default_key = Path.home() / ".ssh" / "inovxio_deploy"
    if default_key.is_file():
        return default_key
    raise RobotOpsError("未配置运维跳板私钥")


def _connect_jump(cfg: dict[str, str], timeout_s: int = 8) -> Any:
    paramiko = _require_paramiko()
    host = (cfg.get("DOSO_OPS_JUMP_HOST") or "47.103.89.137").strip()
    port = int(cfg.get("DOSO_OPS_JUMP_PORT") or 22)
    user = (cfg.get("DOSO_OPS_JUMP_USER") or "admin").strip()
    key_path = _resolve_jump_key(cfg)

    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        client.connect(
            hostname=host,
            port=port,
            username=user,
            key_filename=str(key_path),
            timeout=timeout_s,
            banner_timeout=timeout_s,
            auth_timeout=timeout_s,
            look_for_keys=False,
            allow_agent=False,
        )
        return client
    except Exception as exc:
        client.close()
        raise RobotOpsError(f"连接云端运维跳板失败: {exc}") from exc


def _robot_auth_kwargs(cfg: dict[str, str]) -> dict[str, Any]:
    kwargs: dict[str, Any] = {
        "username": (cfg.get("DOSO_OPS_ROBOT_USER") or "sunrise").strip() or "sunrise",
        "look_for_keys": False,
        "allow_agent": False,
    }
    key_path = (cfg.get("DOSO_OPS_ROBOT_KEY") or "").strip()
    password = (cfg.get("DOSO_OPS_ROBOT_PASSWORD") or "").strip()
    if key_path:
        p = Path(key_path).expanduser()
        if not p.is_file():
            raise RobotOpsError(f"机器狗私钥不存在: {p}")
        kwargs["key_filename"] = str(p)
    if password:
        kwargs["password"] = password
    if "key_filename" not in kwargs and "password" not in kwargs:
        raise RobotOpsError("请先在「云端配置」填入机器狗运维密码或私钥")
    return kwargs


@contextlib.contextmanager
def open_robot_ssh(cfg: dict[str, str], target: dict[str, Any], timeout_s: int = 10) -> Iterator[Any]:
    paramiko = _require_paramiko()
    mode = str(target.get("mode") or "tunnel").strip()
    auth = _robot_auth_kwargs(cfg)

    jump_client: Any = None
    robot_client = paramiko.SSHClient()
    robot_client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        if mode == "tunnel":
            tunnel_port = int(target.get("port") or 0)
            if not (22000 <= tunnel_port <= 22999):
                raise RobotOpsError("隧道端口必须在 22000–22999 之间")
            jump_client = _connect_jump(cfg, timeout_s=timeout_s)
            transport = jump_client.get_transport()
            if transport is None:
                raise RobotOpsError("云端跳板会话不可用")
            chan = transport.open_channel(
                "direct-tcpip",
                ("127.0.0.1", tunnel_port),
                ("127.0.0.1", 0),
                timeout=timeout_s,
            )
            robot_client.connect(
                hostname="127.0.0.1",
                port=tunnel_port,
                sock=chan,
                timeout=timeout_s,
                banner_timeout=timeout_s,
                auth_timeout=timeout_s,
                **auth,
            )
        elif mode == "direct":
            host = str(target.get("host") or "").strip()
            port = int(target.get("port") or 22)
            if not host or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", host):
                raise RobotOpsError("直连地址无效")
            if not (1 <= port <= 65535):
                raise RobotOpsError("直连端口无效")
            robot_client.connect(
                hostname=host,
                port=port,
                timeout=timeout_s,
                banner_timeout=timeout_s,
                auth_timeout=timeout_s,
                **auth,
            )
        else:
            raise RobotOpsError(f"不支持的连接模式: {mode}")
        yield robot_client
    except RobotOpsError:
        raise
    except Exception as exc:
        raise RobotOpsError(f"连接机器狗失败: {exc}") from exc
    finally:
        try:
            robot_client.close()
        except Exception:
            pass
        if jump_client is not None:
            try:
                jump_client.close()
            except Exception:
                pass


def _exec_cmd(client: Any, cmd: str, timeout_s: int = 12, stdin_text: str | None = None) -> tuple[int, str, str]:
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout_s)
    if stdin_text is not None:
        stdin.write(stdin_text)
        stdin.flush()
    stdin.channel.shutdown_write()
    out = stdout.read().decode("utf-8", errors="replace")
    err = stderr.read().decode("utf-8", errors="replace")
    code = stdout.channel.recv_exit_status()
    return code, out, err


def _exec_sudo(client: Any, cfg: dict[str, str], inner_cmd: str, timeout_s: int = 15) -> tuple[int, str, str]:
    password = (cfg.get("DOSO_OPS_ROBOT_PASSWORD") or "").strip()
    if password:
        return _exec_cmd(
            client,
            f"sudo -S -p '' bash -lc {shlex.quote(inner_cmd)}",
            timeout_s=timeout_s,
            stdin_text=password + "\n",
        )
    return _exec_cmd(client, f"sudo -n bash -lc {shlex.quote(inner_cmd)}", timeout_s=timeout_s)


def discover_tunnels(cfg: dict[str, str]) -> dict[str, Any]:
    jump = _connect_jump(cfg, timeout_s=8)
    try:
        code, out, err = _exec_cmd(jump, "ss -tln | awk '{print $4}'", timeout_s=8)
        if code != 0:
            raise RobotOpsError(f"查询隧道端口失败: {err.strip()}")
        ports: set[int] = set()
        for line in out.splitlines():
            m = re.search(r"(?:127\.0\.0\.1|\[::1\]):(\d+)$", line.strip())
            if m:
                p = int(m.group(1))
                if 22001 <= p <= 22999:
                    ports.add(p)
        items = []
        for p in sorted(ports):
            idx = p - 22000
            items.append(
                {
                    "mode": "tunnel",
                    "port": p,
                    "label": f"{idx} 号机 (:{p})",
                }
            )
        return {
            "jumpHost": (cfg.get("DOSO_OPS_JUMP_HOST") or "47.103.89.137").strip(),
            "tunnels": items,
            "quickDirs": list(QUICK_DIRS),
            "services": list(ALLOWED_SERVICES),
        }
    finally:
        jump.close()


def get_robot_status(cfg: dict[str, str], target: dict[str, Any]) -> dict[str, Any]:
    with open_robot_ssh(cfg, target) as client:
        svc_args = " ".join(ALLOWED_SERVICES)
        script = (
            "printf '===HOST===\\n'; hostname; "
            "printf '===UPTIME===\\n'; uptime -p 2>/dev/null || uptime; "
            "printf '===LOAD===\\n'; cat /proc/loadavg; "
            "printf '===MEM===\\n'; free -b | awk '/^Mem:/ {print $2, $3, $7}'; "
            "printf '===DISK===\\n'; df -B1 / /opt/doso 2>/dev/null | tail -n +2; "
            f"printf '===SVCS===\\n'; for s in {svc_args}; do printf '%s=%s\\n' \"$s\" \"$(systemctl is-active \"$s\" 2>/dev/null || true)\"; done; "
            "printf '===HEALTH===\\n'; curl -s --max-time 3 http://127.0.0.1:5054/health || echo '{}'"
        )
        _, out, _ = _exec_cmd(client, script, timeout_s=12)

    sections: dict[str, list[str]] = {}
    cur = ""
    for line in out.splitlines():
        m = re.fullmatch(r"===([A-Z]+)===", line.strip())
        if m:
            cur = m.group(1)
            sections[cur] = []
        elif cur:
            sections[cur].append(line)

    health_raw = "\n".join(sections.get("HEALTH", [])).strip()
    try:
        health = json.loads(health_raw) if health_raw else {}
    except ValueError:
        health = {}

    services: list[dict[str, str]] = []
    for line in sections.get("SVCS", []):
        if "=" in line:
            k, v = line.split("=", 1)
            services.append({"unit": k.strip(), "state": v.strip() or "unknown"})

    mem_parts = (" ".join(sections.get("MEM", []))).split()
    mem = None
    if len(mem_parts) >= 3 and all(p.isdigit() for p in mem_parts[:3]):
        mem = {
            "totalBytes": int(mem_parts[0]),
            "usedBytes": int(mem_parts[1]),
            "availBytes": int(mem_parts[2]),
        }

    disk = None
    for dline in sections.get("DISK", []):
        parts = dline.split()
        if len(parts) >= 6 and parts[1].isdigit() and parts[2].isdigit() and parts[3].isdigit():
            disk = {
                "mount": parts[5],
                "totalBytes": int(parts[1]),
                "usedBytes": int(parts[2]),
                "availBytes": int(parts[3]),
                "usePercent": parts[4],
            }
            break

    return {
        "hostname": (sections.get("HOST") or [""])[0].strip(),
        "uptime": (sections.get("UPTIME") or [""])[0].strip(),
        "loadAvg": (sections.get("LOAD") or [""])[0].strip(),
        "memory": mem,
        "disk": disk,
        "services": services,
        "health": health,
    }


def control_service(cfg: dict[str, str], target: dict[str, Any], unit: str, action: str) -> dict[str, Any]:
    clean_unit = validate_service(unit)
    clean_action = validate_service_action(action)

    with open_robot_ssh(cfg, target) as client:
        if clean_unit == "doso-brainstem.service" and clean_action in ("restart", "stop"):
            _, h_out, _ = _exec_cmd(client, "curl -s --max-time 3 http://127.0.0.1:5054/health || echo '{}'", timeout_s=6)
            try:
                h = json.loads(h_out.strip() or "{}")
            except ValueError:
                h = {}
            if h.get("motor_output_enabled") is True:
                raise RobotOpsError("电机使能开启中，禁止停止或重启主控")

        code, out, err = _exec_sudo(client, cfg, f"systemctl {clean_action} {shlex.quote(clean_unit)}", timeout_s=20)
        if code != 0:
            raise RobotOpsError(f"执行失败: {(err or out).strip()}")
        _, st_out, _ = _exec_cmd(client, f"systemctl is-active {shlex.quote(clean_unit)} || true", timeout_s=5)
        return {"unit": clean_unit, "action": clean_action, "state": st_out.strip() or "unknown"}


def get_service_logs(
    cfg: dict[str, str],
    target: dict[str, Any],
    unit: str,
    lines: int = 100,
    keyword: str = "",
) -> dict[str, Any]:
    clean_unit = validate_service(unit)
    n = max(10, min(500, int(lines or 100)))
    with open_robot_ssh(cfg, target) as client:
        code, out, err = _exec_sudo(
            client,
            cfg,
            f"journalctl -u {shlex.quote(clean_unit)} -n {n} --no-pager",
            timeout_s=12,
        )
        if code != 0:
            raise RobotOpsError(f"读取日志失败: {(err or out).strip()}")
    text = out
    kw = (keyword or "").strip().lower()
    if kw:
        text = "\n".join(line for line in text.splitlines() if kw in line.lower())
    return {"unit": clean_unit, "lines": n, "text": text}


def list_dir(cfg: dict[str, str], target: dict[str, Any], remote_path: str) -> dict[str, Any]:
    norm = normalize_remote_path(remote_path)
    with open_robot_ssh(cfg, target) as client:
        sftp = client.open_sftp()
        try:
            attrs = sftp.listdir_attr(norm)
        except Exception as exc:
            raise RobotOpsError(f"无法读取目录 {norm}: {exc}") from exc
        finally:
            sftp.close()

    entries: list[dict[str, Any]] = []
    for attr in attrs:
        name = attr.filename
        mode = attr.st_mode or 0
        is_dir = stat.S_ISDIR(mode)
        is_link = stat.S_ISLNK(mode)
        full = posixpath.join(norm, name)
        entries.append(
            {
                "name": name,
                "path": full,
                "isDir": is_dir,
                "isLink": is_link,
                "sizeBytes": int(attr.st_size or 0),
                "mode": oct(stat.S_IMODE(mode))[2:].zfill(3),
                "modifiedAtMs": int((attr.st_mtime or 0) * 1000),
            }
        )
    entries.sort(key=lambda e: (not e["isDir"], e["name"].lower()))
    parent = posixpath.dirname(norm) if norm != "/" else None
    return {"path": norm, "parent": parent, "entries": entries}


def read_text_file(cfg: dict[str, str], target: dict[str, Any], remote_path: str) -> dict[str, Any]:
    norm = normalize_remote_path(remote_path)
    with open_robot_ssh(cfg, target) as client:
        sftp = client.open_sftp()
        try:
            st = sftp.stat(norm)
            if stat.S_ISDIR(st.st_mode or 0):
                raise RobotOpsError("不能以文本方式打开目录")
            if (st.st_size or 0) > MAX_TEXT_EDIT_BYTES:
                raise RobotOpsError("文件超过 512 KB，请使用下载")
            with sftp.open(norm, "rb") as fh:
                raw = fh.read(MAX_TEXT_EDIT_BYTES + 1)
        except PermissionError:
            sftp.close()
            code, out, err = _exec_sudo(client, cfg, f"head -c {MAX_TEXT_EDIT_BYTES + 1} {shlex.quote(norm)}", timeout_s=10)
            if code != 0:
                raise RobotOpsError(f"读取文件失败: {err.strip()}")
            raw = out.encode("utf-8")
        except RobotOpsError:
            raise
        except Exception as exc:
            raise RobotOpsError(f"读取文件失败: {exc}") from exc
        else:
            sftp.close()

    if len(raw) > MAX_TEXT_EDIT_BYTES:
        raise RobotOpsError("文件超过 512 KB，请使用下载")
    try:
        content = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise RobotOpsError("该文件不是 UTF-8 文本，请使用下载") from exc
    return {"path": norm, "sizeBytes": len(raw), "content": content}


def write_text_file(cfg: dict[str, str], target: dict[str, Any], remote_path: str, content: str) -> dict[str, Any]:
    norm = normalize_remote_path(remote_path)
    if norm in PROTECTED_DELETE_PATHS:
        raise RobotOpsError("禁止直接覆盖系统关键路径")
    encoded = content.encode("utf-8")
    if len(encoded) > MAX_TEXT_EDIT_BYTES:
        raise RobotOpsError("保存内容超过 512 KB 上限")

    ts = time.strftime("%Y%m%d%H%M%S")
    with open_robot_ssh(cfg, target) as client:
        sftp = client.open_sftp()
        tmp_remote = f"/tmp/.doso-edit-{os.getpid()}-{int(time.time() * 1000)}"
        try:
            with sftp.open(tmp_remote, "wb") as fh:
                fh.write(encoded)
        finally:
            sftp.close()

        inner = (
            f"if test -f {shlex.quote(norm)}; then cp -a {shlex.quote(norm)} {shlex.quote(norm + '.bak.' + ts)}; fi; "
            f"cat {shlex.quote(tmp_remote)} > {shlex.quote(norm)} && rm -f {shlex.quote(tmp_remote)}"
        )
        code, _, err = _exec_sudo(client, cfg, inner, timeout_s=12)
        if code != 0:
            _exec_cmd(client, f"rm -f {shlex.quote(tmp_remote)}", timeout_s=5)
            raise RobotOpsError(f"保存文件失败: {err.strip()}")
    return {"path": norm, "sizeBytes": len(encoded), "saved": True}


def upload_file(
    cfg: dict[str, str],
    target: dict[str, Any],
    remote_dir: str,
    local_path: Path,
    filename: str,
) -> dict[str, Any]:
    norm_dir = normalize_remote_path(remote_dir)
    safe_name = Path(filename.replace("\\", "/")).name.strip()
    if not safe_name or safe_name in (".", "..") or "/" in safe_name:
        raise RobotOpsError("文件名无效")
    dest = posixpath.join(norm_dir, safe_name)
    size = local_path.stat().st_size

    with open_robot_ssh(cfg, target, timeout_s=15) as client:
        sftp = client.open_sftp()
        try:
            sftp.put(str(local_path), dest)
        except PermissionError:
            tmp_remote = f"/tmp/.doso-up-{int(time.time() * 1000)}_{safe_name}"
            sftp.put(str(local_path), tmp_remote)
            sftp.close()
            code, _, err = _exec_sudo(
                client,
                cfg,
                f"mv -f {shlex.quote(tmp_remote)} {shlex.quote(dest)}",
                timeout_s=15,
            )
            if code != 0:
                _exec_cmd(client, f"rm -f {shlex.quote(tmp_remote)}", timeout_s=5)
                raise RobotOpsError(f"写入目标目录失败: {err.strip()}")
        except Exception as exc:
            sftp.close()
            raise RobotOpsError(f"上传失败: {exc}") from exc
        else:
            sftp.close()
    return {"path": dest, "sizeBytes": size, "uploaded": True}


def download_file_bytes(cfg: dict[str, str], target: dict[str, Any], remote_path: str) -> tuple[str, bytes]:
    norm = normalize_remote_path(remote_path)
    filename = posixpath.basename(norm) or "download.bin"
    with open_robot_ssh(cfg, target, timeout_s=15) as client:
        sftp = client.open_sftp()
        try:
            st = sftp.stat(norm)
            if stat.S_ISDIR(st.st_mode or 0):
                raise RobotOpsError("不支持直接下载目录")
            if (st.st_size or 0) > MAX_DOWNLOAD_BYTES:
                raise RobotOpsError("文件超过 128 MB 下载上限")
            with sftp.open(norm, "rb") as fh:
                data = fh.read()
        except RobotOpsError:
            raise
        except Exception as exc:
            raise RobotOpsError(f"下载失败: {exc}") from exc
        finally:
            sftp.close()
    return filename, data


def make_dir(cfg: dict[str, str], target: dict[str, Any], remote_path: str) -> dict[str, Any]:
    norm = normalize_remote_path(remote_path)
    with open_robot_ssh(cfg, target) as client:
        sftp = client.open_sftp()
        try:
            sftp.mkdir(norm)
        except PermissionError:
            sftp.close()
            code, _, err = _exec_sudo(client, cfg, f"mkdir -p {shlex.quote(norm)}", timeout_s=10)
            if code != 0:
                raise RobotOpsError(f"创建目录失败: {err.strip()}")
        except Exception as exc:
            sftp.close()
            raise RobotOpsError(f"创建目录失败: {exc}") from exc
        else:
            sftp.close()
    return {"path": norm, "created": True}


def delete_path(cfg: dict[str, str], target: dict[str, Any], remote_path: str) -> dict[str, Any]:
    norm = normalize_remote_path(remote_path)
    if norm in PROTECTED_DELETE_PATHS:
        raise RobotOpsError(f"受保护的系统关键路径禁止删除: {norm}")
    with open_robot_ssh(cfg, target) as client:
        sftp = client.open_sftp()
        try:
            st = sftp.lstat(norm)
            is_dir = stat.S_ISDIR(st.st_mode or 0)
            if is_dir:
                sftp.rmdir(norm)
            else:
                sftp.remove(norm)
        except PermissionError:
            sftp.close()
            code, _, err = _exec_sudo(
                client,
                cfg,
                f"if test -d {shlex.quote(norm)}; then rmdir {shlex.quote(norm)}; else rm -f {shlex.quote(norm)}; fi",
                timeout_s=10,
            )
            if code != 0:
                raise RobotOpsError(f"删除失败（非空目录需先清空）: {err.strip()}")
        except Exception as exc:
            sftp.close()
            raise RobotOpsError(f"删除失败: {exc}") from exc
        else:
            sftp.close()
    return {"path": norm, "deleted": True}
