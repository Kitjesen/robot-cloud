#!/usr/bin/env python3
"""Unified fleet aggregation, anomaly diagnostics, fleet patrol, and account RBAC ops.

Combines three live cloud data planes into one coherent admin view:
1. SWAS Supabase business inventory + MQTT remote state telemetry (public.devices + private.device_remote_state)
2. doso-ota-release registry, live heartbeats, releases, and persisted job history
3. SWAS 443-WSS ops reverse tunnels (127.0.0.1:22001-22999) + concurrent live patrol probes
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import json
import re
import time
from typing import Any

import robot_ops


ALLOWED_APP_ROLES = ("user", "support", "admin")
PRIMARY_ADMIN_EMAIL = "995632531@qq.com"
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.IGNORECASE)
EMAIL_RE = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
SEMVER_RE = re.compile(r"v?(\d+)\.(\d+)\.(\d+)")
ACTIVE_JOB_PHASES = frozenset({"scheduled", "downloading", "verifying", "waiting_safe_window", "installing", "restarting"})


class FleetOpsError(ValueError):
    """Actionable fleet management error."""


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _parse_semver_tuple(raw: str | None) -> tuple[int, int, int] | None:
    if not raw:
        return None
    m = SEMVER_RE.search(str(raw))
    if not m:
        return None
    return int(m.group(1)), int(m.group(2)), int(m.group(3))


def _infer_robot_number(
    *,
    serial_number: str = "",
    tags: list[str] | None = None,
    display_name: str = "",
) -> int | None:
    for tag in tags or []:
        m = re.fullmatch(r"dog(\d{1,3})", str(tag).strip().lower())
        if m:
            return int(m.group(1))
    m_ser = re.search(r"(?:DEV|DOG|THUNDER)[-_](\d{1,3})$", serial_number.strip(), re.IGNORECASE)
    if m_ser:
        return int(m_ser.group(1))
    m_name = re.search(r"(\d{1,3})\s*$", display_name.strip())
    if m_name:
        return int(m_name.group(1))
    return None


def fetch_swas_fleet_snapshot(cfg: dict[str, str]) -> dict[str, Any]:
    """Fetch business inventory, MQTT remote_state, and active 443 tunnel ports in one SSH call."""
    empty = {"available": False, "inventory": [], "tunnelPorts": [], "error": None}
    try:
        jump = robot_ops._connect_jump(cfg, timeout_s=7)
    except Exception as exc:
        empty["error"] = str(exc)
        return empty

    sql = """
SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json)::text
FROM (
  SELECT
    d.id::text AS "deviceId",
    COALESCE(d.serial_number, '') AS "serialNumber",
    COALESCE(d.display_name, '') AS "displayName",
    COALESCE(d.model_code, '') AS "modelCode",
    d.is_simulated AS "isSimulated",
    COALESCE(d.owner_id::text, '') AS "ownerId",
    COALESCE(p.email, '') AS "ownerEmail",
    CASE WHEN r.device_id IS NULL THEN NULL ELSE json_build_object(
      'enabled', r.enabled,
      'gatewayReachable', r.gateway_reachable,
      'lastSeenAt', r.last_seen_at,
      'cmsState', COALESCE(r.latest->'report'->'status'->'state'->>'kind', ''),
      'motorOutputEnabled', (r.latest->'report'->'status'->'control'->>'motorOutputEnabled')::boolean,
      'criticalMotorFault', (r.latest->'report'->'status'->'control'->>'criticalMotorFault')::boolean
    ) END AS "remoteState"
  FROM public.devices d
  LEFT JOIN public.profiles p ON p.id = d.owner_id
  LEFT JOIN private.device_remote_state r ON r.device_id = d.id
  WHERE d.is_demo = false
  ORDER BY d.is_simulated ASC, d.created_at ASC
) t;
"""
    try:
        cmd = (
            "printf '===INV===\\n'; "
            "sudo -n docker exec -i supabase-db psql -At -U postgres -d postgres; "
            "printf '\\n===PORTS===\\n'; "
            "ss -tln | awk '{print $4}'"
        )
        code, out, err = robot_ops._exec_cmd(jump, cmd, timeout_s=10, stdin_text=sql.strip() + "\n")
        if code != 0 and "===INV===" not in out:
            empty["error"] = (err or out).strip()
            return empty

        inv_part = ""
        ports_part = ""
        if "===INV===" in out:
            after_inv = out.split("===INV===", 1)[1]
            if "===PORTS===" in after_inv:
                inv_part, ports_part = after_inv.split("===PORTS===", 1)
            else:
                inv_part = after_inv

        inventory: list[dict[str, Any]] = []
        inv_text = inv_part.strip()
        if inv_text:
            try:
                parsed = json.loads(inv_text)
                if isinstance(parsed, list):
                    inventory = parsed
            except json.JSONDecodeError:
                pass

        ports: set[int] = set()
        for line in ports_part.splitlines():
            m = re.search(r"(?:127\.0\.0\.1|\[::1\]):(\d+)$", line.strip())
            if m:
                port = int(m.group(1))
                if 22001 <= port <= 22999:
                    ports.add(port)

        return {
            "available": True,
            "inventory": inventory,
            "tunnelPorts": sorted(ports),
            "error": None,
        }
    except Exception as exc:
        empty["error"] = str(exc)
        return empty
    finally:
        jump.close()


def diagnose_device(
    dev: dict[str, Any],
    latest_stable_versions: dict[tuple[str, str], str],
) -> tuple[str, list[dict[str, str]]]:
    """Compute health level ('critical'|'warn'|'ok'|'offline') and structured anomaly list."""
    anomalies: list[dict[str, str]] = []
    is_sim = bool(dev.get("isSimulated"))
    ota_reg = bool(dev.get("otaRegistered"))
    ota_st = dev.get("otaStatus") or {}
    ota_age = ota_st.get("lastSeenAgeMs")
    ota_online = isinstance(ota_age, (int, float)) and ota_age <= 120_000
    remote = dev.get("remoteState") or {}
    cloud_enabled = bool(remote.get("enabled"))
    cloud_reachable = bool(remote.get("gatewayReachable"))
    tunnel_port = dev.get("tunnelPort")
    latest_job = dev.get("latestJob") or {}

    # 1. Hardware / motion critical fault from MQTT remote_state telemetry (supports both compact scalar & raw latest)
    report_status = ((remote.get("latest") or {}).get("report") or {}).get("status") or {}
    ctrl = report_status.get("control") or {}
    crit_fault = remote.get("criticalMotorFault")
    if crit_fault is None:
        crit_fault = ctrl.get("criticalMotorFault")
    if crit_fault is True:
        anomalies.append(
            {
                "code": "critical_motor_fault",
                "level": "critical",
                "label": "电机严重故障",
                "detail": "云端遥测上报关键电机故障标志，请立即排查硬件与 CAN 总线",
            }
        )

    # 2. OTA job failure / rollback / recovery / waiting safe window
    job_phase = str(latest_job.get("phase") or "")
    job_msg = str(latest_job.get("message") or "").strip()
    if job_phase == "recovery_required":
        anomalies.append(
            {
                "code": "ota_recovery_required",
                "level": "critical",
                "label": "OTA 需人工恢复",
                "detail": job_msg or f"任务 {latest_job.get('releaseId', '')} 处于恢复状态",
            }
        )
    elif job_phase in ("failed", "rolled_back"):
        anomalies.append(
            {
                "code": "ota_failed",
                "level": "critical",
                "label": "升级失败已回滚" if job_phase == "rolled_back" else "最近升级失败",
                "detail": job_msg or f"版本 {latest_job.get('releaseId', '')} 未能完成安装",
            }
        )
    elif job_phase == "waiting_safe_window":
        anomalies.append(
            {
                "code": "ota_waiting_safe_window",
                "level": "warn",
                "label": "等待安全窗",
                "detail": job_msg or "新版本已下载校验完成，等待电机下使能后自动安装",
            }
        )

    # 3. On-robot runtime health reported by doso-ota heartbeat
    if ota_online:
        if ota_st.get("brainstemReady") is False:
            anomalies.append(
                {
                    "code": "brainstem_not_ready",
                    "level": "critical",
                    "label": "主控未就绪",
                    "detail": "doso-brainstem 健康接口报告未就绪",
                }
            )
        if ota_st.get("strategyReady") is False:
            anomalies.append(
                {
                    "code": "strategy_not_ready",
                    "level": "warn",
                    "label": "策略未加载",
                    "detail": "运动策略引擎未就绪",
                }
            )
        if ota_st.get("canReadOnly") is True:
            anomalies.append(
                {
                    "code": "can_read_only",
                    "level": "warn",
                    "label": "CAN 只读模式",
                    "detail": "当前 CAN 处于只读保护模式",
                }
            )

    # 4. Link completeness & connectivity checks (real devices)
    if not is_sim:
        if dev.get("inInventory") and not ota_reg:
            anomalies.append(
                {
                    "code": "ota_unregistered",
                    "level": "warn",
                    "label": "未接入 OTA",
                    "detail": "已在业务库存登记，但尚未接入云端 OTA 服务",
                }
            )
        if ota_reg and not ota_online:
            anomalies.append(
                {
                    "code": "ota_offline",
                    "level": "warn" if (cloud_reachable or tunnel_port) else "offline",
                    "label": "OTA 心跳离线",
                    "detail": "超过 2 分钟未收到机载 OTA 心跳" if ota_age is not None else "尚未收到首次 OTA 心跳",
                }
            )
        if cloud_enabled and not cloud_reachable and (ota_online or tunnel_port):
            anomalies.append(
                {
                    "code": "cloud_unreachable",
                    "level": "warn",
                    "label": "云状态断开",
                    "detail": "机器在线，但 MQTT 云端状态通道不可达",
                }
            )
        if not tunnel_port and (ota_online or cloud_reachable):
            anomalies.append(
                {
                    "code": "tunnel_offline",
                    "level": "warn",
                    "label": "运维隧道未连",
                    "detail": "机器在线但未检测到 443 运维隧道端口",
                }
            )

    # 5. Outdated firmware check against latest stable brainstem release
    if ota_online and not is_sim:
        cur_v = _parse_semver_tuple(ota_st.get("firmwareVersion"))
        model = str(dev.get("model") or "")
        hw = str(dev.get("hardwareRevision") or "")
        latest_raw = latest_stable_versions.get((model, hw)) or latest_stable_versions.get((model, ""))
        latest_v = _parse_semver_tuple(latest_raw)
        if cur_v and latest_v and cur_v < latest_v:
            anomalies.append(
                {
                    "code": "firmware_outdated",
                    "level": "info",
                    "label": f"可升至 v{latest_raw}",
                    "detail": f"当前版本 {ota_st.get('firmwareVersion')} 落后于正式版 {latest_raw}",
                }
            )

    levels = {a["level"] for a in anomalies}
    any_online = bool(ota_online or cloud_reachable or tunnel_port)
    if "critical" in levels:
        health = "critical"
    elif "warn" in levels:
        health = "warn"
    elif any_online:
        health = "ok"
    else:
        health = "offline"
    return health, anomalies


def build_unified_fleet(
    *,
    registered_devices: list[dict[str, Any]],
    ota_statuses: list[dict[str, Any]],
    jobs: list[dict[str, Any]],
    releases: list[dict[str, Any]],
    swas_snapshot: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Merge business inventory, OTA registry/status, persisted jobs, and 443 tunnels."""
    snap = swas_snapshot or {"available": False, "inventory": [], "tunnelPorts": []}
    inv_by_id = {str(d["deviceId"]): d for d in (snap.get("inventory") or []) if d.get("deviceId")}
    reg_by_id = {str(d["deviceId"]): d for d in (registered_devices or []) if d.get("deviceId")}
    st_by_id = {str(d["deviceId"]): d for d in (ota_statuses or []) if d.get("deviceId")}
    online_tunnels: set[int] = set(int(p) for p in (snap.get("tunnelPorts") or []))

    # Group persisted jobs per device (already newest-first from cloud, re-sort defensively)
    jobs_by_dev: dict[str, list[dict[str, Any]]] = {}
    for j in jobs or []:
        did = str(j.get("deviceId") or "")
        if did:
            jobs_by_dev.setdefault(did, []).append(j)
    for dev_jobs in jobs_by_dev.values():
        dev_jobs.sort(
            key=lambda x: max(int(x.get("updatedAtMs") or 0), int(x.get("createdAtMs") or 0)),
            reverse=True,
        )

    # Latest stable brainstem version per (model, hardwareRevision)
    latest_stable: dict[tuple[str, str], tuple[int, str]] = {}
    for rel in releases or []:
        if str(rel.get("channel") or "").lower() != "stable":
            continue
        if str(rel.get("artifactType") or "") not in ("brainstem_release", "release_bundle"):
            continue
        seq = int(rel.get("sequence") or 0)
        ver = str(rel.get("version") or "")
        model = str(rel.get("model") or "")
        hw = str(rel.get("hardwareRevision") or "")
        for key in ((model, hw), (model, "")):
            if key not in latest_stable or seq > latest_stable[key][0]:
                latest_stable[key] = (seq, ver)
    latest_stable_versions = {k: v[1] for k, v in latest_stable.items()}

    all_ids = list(dict.fromkeys([*inv_by_id.keys(), *reg_by_id.keys(), *st_by_id.keys()]))
    matched_ports: set[int] = set()
    fleet: list[dict[str, Any]] = []

    for did in all_ids:
        inv = inv_by_id.get(did)
        reg = reg_by_id.get(did)
        st = st_by_id.get(did)
        dev_jobs = jobs_by_dev.get(did, [])
        latest_job = dev_jobs[0] if dev_jobs else None

        serial = str((inv or {}).get("serialNumber") or "")
        display_name = str((inv or {}).get("displayName") or "")
        tags = list((reg or {}).get("tags") or [])
        robot_num = _infer_robot_number(serial_number=serial, tags=tags, display_name=display_name)
        candidate_port = (22000 + robot_num) if (robot_num and 1 <= robot_num <= 999) else None
        tunnel_port = candidate_port if (candidate_port in online_tunnels) else None
        if tunnel_port is not None:
            matched_ports.add(tunnel_port)

        remote = (inv or {}).get("remoteState") or None
        report_status = (((remote or {}).get("latest") or {}).get("report") or {}).get("status") or {}
        raw_cms = (remote or {}).get("cmsState") or ((report_status.get("state") or {}).get("kind") or "")
        cms_state = str(raw_cms).replace("CMS_STATE_KIND_", "").lower()
        motor_enabled = (remote or {}).get("motorOutputEnabled")
        if motor_enabled is None:
            motor_enabled = (report_status.get("control") or {}).get("motorOutputEnabled")
        critical_fault = (remote or {}).get("criticalMotorFault")
        if critical_fault is None:
            critical_fault = (report_status.get("control") or {}).get("criticalMotorFault")
        location = report_status.get("location")

        ota_age = (st or {}).get("lastSeenAgeMs")
        ota_online = isinstance(ota_age, (int, float)) and ota_age <= 120_000
        cloud_reachable = bool((remote or {}).get("gatewayReachable"))

        entry: dict[str, Any] = {
            "deviceId": did,
            "robotNumber": robot_num,
            "displayName": display_name or (f"机器狗 #{robot_num}" if robot_num else did[:8]),
            "serialNumber": serial,
            "ownerId": str((inv or {}).get("ownerId") or ""),
            "ownerEmail": str((inv or {}).get("ownerEmail") or ""),
            "ownerName": str((inv or {}).get("ownerName") or ""),
            "isSimulated": bool((inv or {}).get("isSimulated", False)),
            "inInventory": inv is not None,
            "otaRegistered": reg is not None,
            "model": str((reg or {}).get("model") or (inv or {}).get("modelCode") or ""),
            "hardwareRevision": str((reg or {}).get("hardwareRevision") or ""),
            "ring": str((reg or {}).get("ring") or "prod"),
            "enabled": (reg or {}).get("enabled", True) is not False,
            "tags": tags,
            "otaOnline": ota_online,
            "otaStatus": st,
            "cloudReachable": cloud_reachable,
            "remoteState": remote,
            "telemetry": {
                "cmsState": cms_state or None,
                "motorOutputEnabled": motor_enabled,
                "criticalMotorFault": critical_fault,
                "location": location,
                "lastSeenAt": (remote or {}).get("lastSeenAt"),
            },
            "tunnelPort": tunnel_port,
            "expectedTunnelPort": candidate_port,
            "latestJob": latest_job,
            "recentJobs": dev_jobs[:15],
            "activeJob": next((j for j in dev_jobs if str(j.get("phase") or "") in ACTIVE_JOB_PHASES), None),
        }
        health_level, anomalies = diagnose_device(entry, latest_stable_versions)
        entry["healthLevel"] = health_level
        entry["anomalies"] = anomalies
        entry["hasAnomaly"] = any(a["level"] in ("critical", "warn") for a in anomalies)
        fleet.append(entry)

    # Sort: real devices first, then critical -> warn -> ok -> offline, then display name
    rank = {"critical": 0, "warn": 1, "ok": 2, "offline": 3}
    fleet.sort(key=lambda d: (1 if d["isSimulated"] else 0, rank.get(d["healthLevel"], 9), d["displayName"]))

    real = [d for d in fleet if not d["isSimulated"]]
    summary = {
        "totalReal": len(real),
        "totalAll": len(fleet),
        "simulatedCount": len(fleet) - len(real),
        "onlineCount": sum(1 for d in real if d["otaOnline"] or d["cloudReachable"] or d["tunnelPort"]),
        "anomalyCount": sum(1 for d in real if d["hasAnomaly"]),
        "criticalCount": sum(1 for d in real if d["healthLevel"] == "critical"),
        "warnCount": sum(1 for d in real if d["healthLevel"] == "warn"),
        "tunnelCount": len(online_tunnels),
        "unmatchedTunnels": sorted(online_tunnels - matched_ports),
        "inventoryConnected": bool(snap.get("available")),
    }
    return {"fleet": fleet, "summary": summary, "tunnelPorts": sorted(online_tunnels)}


def patrol_fleet(
    cfg: dict[str, str],
    fleet_entries: list[dict[str, Any]] | None = None,
    port_credentials: dict[str, dict[str, str]] | None = None,
) -> dict[str, Any]:
    """Concurrently inspect every online 443-tunnel robot for service, disk, voltage, and 1h error logs."""
    discovered = robot_ops.discover_tunnels(cfg)
    tunnels = discovered.get("tunnels") or []
    by_port = {int(d["tunnelPort"]): d for d in (fleet_entries or []) if d.get("tunnelPort")}
    creds = port_credentials or {}

    def _probe_one(tun: dict[str, Any]) -> dict[str, Any]:
        port = int(tun["port"])
        matched = by_port.get(port)
        cred = creds.get(str(port)) or {}
        target: dict[str, Any] = {"mode": "tunnel", "port": port}
        if cred.get("username"):
            target["username"] = cred["username"]
        if cred.get("password"):
            target["password"] = cred["password"]

        item: dict[str, Any] = {
            "port": port,
            "label": tun.get("label") or f":{port}",
            "deviceId": (matched or {}).get("deviceId") or "",
            "displayName": (matched or {}).get("displayName") or tun.get("label") or f":{port}",
            "serialNumber": (matched or {}).get("serialNumber") or "",
            "reachable": False,
            "issues": [],
            "services": [],
            "health": {},
            "disk": None,
            "memory": None,
            "recentErrorCount": 0,
            "recentErrors": [],
            "error": None,
        }
        try:
            # Single SSH session & single compound command per robot to minimize tunnel load.
            svc_args = " ".join(robot_ops.ALLOWED_SERVICES)
            compound = (
                "printf '===HOST===\\n'; hostname; "
                "printf '===UPTIME===\\n'; uptime -p 2>/dev/null || uptime; "
                "printf '===DISK===\\n'; df -B1 / /opt/doso 2>/dev/null | tail -n +2 | head -n 1; "
                f"printf '===SVCS===\\n'; for s in {svc_args}; do printf '%s=%s\\n' \"$s\" \"$(systemctl is-active \"$s\" 2>/dev/null || true)\"; done; "
                "printf '===HEALTH===\\n'; curl -s --max-time 3 http://127.0.0.1:5054/health || echo '{}'; "
                "printf '\\n===ERRS===\\n'; "
                "journalctl --since '1 hour ago' -p err "
                "-u doso-brainstem.service -u doso-ota.service "
                "-u doso-cloud-link.service -u doso-ops-tunnel.service "
                "-n 30 --no-pager -q 2>/dev/null || true"
            )
            with robot_ops.open_robot_ssh(cfg, target, timeout_s=10) as client:
                _, raw_out, _ = robot_ops._exec_sudo(client, cfg, compound, timeout_s=12, target=target)

            secs: dict[str, list[str]] = {}
            cur = ""
            for line in raw_out.splitlines():
                m = re.fullmatch(r"===([A-Z]+)===", line.strip())
                if m:
                    cur = m.group(1)
                    secs[cur] = []
                elif cur:
                    secs[cur].append(line)

            item["reachable"] = True
            item["hostname"] = (secs.get("HOST") or [""])[0].strip()
            item["uptime"] = (secs.get("UPTIME") or [""])[0].strip()
            item["services"] = [
                {"unit": ln.split("=", 1)[0].strip(), "state": ln.split("=", 1)[1].strip() or "unknown"}
                for ln in secs.get("SVCS", [])
                if "=" in ln
            ]
            try:
                item["health"] = json.loads("\n".join(secs.get("HEALTH", [])).strip() or "{}")
            except ValueError:
                item["health"] = {}
            for dline in secs.get("DISK", []):
                parts = dline.split()
                if len(parts) >= 6 and parts[1].isdigit() and parts[3].isdigit():
                    item["disk"] = {
                        "mount": parts[5],
                        "totalBytes": int(parts[1]),
                        "availBytes": int(parts[3]),
                        "usePercent": parts[4],
                    }
                    break
            err_lines = [l for l in secs.get("ERRS", []) if l.strip() and not l.startswith("-- ")]
            item["recentErrorCount"] = len(err_lines)
            item["recentErrors"] = err_lines[-6:]

            issues: list[dict[str, str]] = []
            for svc in item["services"]:
                if svc.get("state") != "active":
                    issues.append(
                        {
                            "level": "critical" if svc.get("unit") == "doso-brainstem.service" else "warn",
                            "label": f"{svc.get('unit')} ({svc.get('state')})",
                        }
                    )
            h = item["health"]
            if h:
                if h.get("brainstem_ready") is False:
                    issues.append({"level": "critical", "label": "主控未就绪"})
                if h.get("strategy_ready") is False:
                    issues.append({"level": "warn", "label": "策略未加载"})
                vbus = h.get("vbus_min_v")
                if isinstance(vbus, (int, float)) and 0 < vbus < 42.0:
                    issues.append({"level": "critical", "label": f"母线欠压 {vbus:.1f}V"})
            disk = item["disk"]
            if disk and isinstance(disk.get("usePercent"), str):
                pct = int(re.sub(r"[^0-9]", "", disk["usePercent"]) or 0)
                if pct >= 85:
                    issues.append({"level": "warn", "label": f"磁盘偏满 {pct}%"})
            if item["recentErrorCount"] > 0:
                issues.append({"level": "warn", "label": f"近1小时 {item['recentErrorCount']} 条错误日志"})

            item["issues"] = issues
            item["level"] = (
                "critical"
                if any(i["level"] == "critical" for i in issues)
                else ("warn" if issues else "ok")
            )
        except Exception as exc:
            item["error"] = str(exc)
            item["level"] = "critical"
            item["issues"] = [{"level": "critical", "label": f"巡检连接失败: {exc}"}]
        return item

    if not tunnels:
        return {"checkedAtMs": int(time.time() * 1000), "total": 0, "issueCount": 0, "items": []}

    with ThreadPoolExecutor(max_workers=min(6, len(tunnels))) as pool:
        results = list(pool.map(_probe_one, tunnels))

    return {
        "checkedAtMs": int(time.time() * 1000),
        "total": len(results),
        "issueCount": sum(1 for r in results if r.get("issues")),
        "items": results,
    }


def fetch_robot_error_logs(
    cfg: dict[str, str],
    target: dict[str, Any],
    hours: int = 2,
    lines: int = 80,
) -> dict[str, Any]:
    """Fetch recent warning/error logs across all 4 DOSO services for quick single-robot diagnosis."""
    h = max(1, min(48, int(hours or 2)))
    n = max(20, min(300, int(lines or 80)))
    cmd = (
        f"journalctl --since '{h} hours ago' -p warning "
        "-u doso-brainstem.service -u doso-ota.service "
        "-u doso-cloud-link.service -u doso-ops-tunnel.service "
        f"-n {n} --no-pager -q 2>/dev/null || true"
    )
    with robot_ops.open_robot_ssh(cfg, target, timeout_s=10) as client:
        code, out, err = robot_ops._exec_sudo(client, cfg, cmd, timeout_s=12, target=target)
        if code != 0 and not out:
            raise FleetOpsError(f"拉取异常日志失败: {err.strip()}")
    clean_lines = [l for l in out.splitlines() if l.strip() and not l.startswith("-- ")]
    return {
        "hours": h,
        "count": len(clean_lines),
        "text": "\n".join(clean_lines) if clean_lines else f"近 {h} 小时内无警告或错误日志",
    }


def list_accounts(cfg: dict[str, str]) -> dict[str, Any]:
    """List Supabase user profiles with app_role and owned device counts."""
    jump = robot_ops._connect_jump(cfg, timeout_s=8)
    sql = """
SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json)::text
FROM (
  SELECT
    p.id::text AS "id",
    COALESCE(p.email, '') AS "email",
    COALESCE(p.display_name, '') AS "displayName",
    COALESCE(p.app_role, 'user') AS "appRole",
    COALESCE(p.account_status, 'active') AS "accountStatus",
    p.created_at AS "createdAt",
    COALESCE((SELECT count(*) FROM public.devices d WHERE d.owner_id = p.id AND d.is_demo = false), 0) AS "deviceCount",
    COALESCE((SELECT array_agg(COALESCE(d.serial_number, d.display_name) ORDER BY d.created_at) FROM public.devices d WHERE d.owner_id = p.id AND d.is_demo = false), ARRAY[]::text[]) AS "devices"
  FROM public.profiles p
  ORDER BY CASE p.app_role WHEN 'admin' THEN 0 WHEN 'support' THEN 1 ELSE 2 END, p.created_at ASC
) t;
"""
    try:
        code, out, err = robot_ops._exec_cmd(
            jump,
            "sudo -n docker exec -i supabase-db psql -At -U postgres -d postgres",
            timeout_s=12,
            stdin_text=sql.strip() + "\n",
        )
        if code != 0:
            raise FleetOpsError(f"读取账号列表失败: {(err or out).strip()}")
        items = json.loads(out.strip() or "[]")
        return {"accounts": items, "primaryAdminEmail": PRIMARY_ADMIN_EMAIL}
    except FleetOpsError:
        raise
    except Exception as exc:
        raise FleetOpsError(f"解析账号列表失败: {exc}") from exc
    finally:
        jump.close()


def update_account_role(cfg: dict[str, str], user_id: str, new_role: str) -> dict[str, Any]:
    """Update a user's app_role ('user'|'support'|'admin') in Supabase public.profiles safely."""
    uid = (user_id or "").strip()
    role = (new_role or "").strip().lower()
    if not UUID_RE.fullmatch(uid):
        raise FleetOpsError("账号 ID 无效")
    if role not in ALLOWED_APP_ROLES:
        raise FleetOpsError(f"不支持的角色: {role}")

    jump = robot_ops._connect_jump(cfg, timeout_s=8)
    try:
        # Check current role & ensure we never remove the last active admin or demote primary admin without intent
        chk_sql = (
            f"SELECT COALESCE(email,''), COALESCE(app_role,'user'), "
            f"(SELECT count(*) FROM public.profiles WHERE app_role = 'admin' AND account_status = 'active') "
            f"FROM public.profiles WHERE id = {_sql_literal(uid)}::uuid;"
        )
        code, out, err = robot_ops._exec_cmd(
            jump,
            "sudo -n docker exec -i supabase-db psql -At -F '|' -U postgres -d postgres",
            timeout_s=10,
            stdin_text=chk_sql + "\n",
        )
        if code != 0 or not out.strip():
            raise FleetOpsError("未找到目标账号")
        email, cur_role, admin_cnt_str = out.strip().splitlines()[0].split("|", 2)
        admin_cnt = int(admin_cnt_str or 0)
        if cur_role == "admin" and role != "admin":
            if email.lower() == PRIMARY_ADMIN_EMAIL.lower():
                raise FleetOpsError(f"主管理员账号 {PRIMARY_ADMIN_EMAIL} 受保护，禁止降级")
            if admin_cnt <= 1:
                raise FleetOpsError("系统至少保留 1 名管理员，无法降级")

        upd_sql = (
            f"UPDATE public.profiles SET app_role = {_sql_literal(role)} "
            f"WHERE id = {_sql_literal(uid)}::uuid "
            f"RETURNING id::text, COALESCE(email,''), app_role;"
        )
        code2, out2, err2 = robot_ops._exec_cmd(
            jump,
            "sudo -n docker exec -i supabase-db psql -v ON_ERROR_STOP=1 -At -F '|' -U postgres -d postgres",
            timeout_s=10,
            stdin_text=upd_sql + "\n",
        )
        if code2 != 0:
            raise FleetOpsError(f"修改角色失败: {(err2 or out2).strip()}")
        return {"ok": True, "id": uid, "email": email, "appRole": role}
    finally:
        jump.close()


def update_device_owner(cfg: dict[str, str], device_id: str, owner_email: str) -> dict[str, Any]:
    """Reassign a robot's owner_id in Supabase public.devices by account email (empty = unbind)."""
    did = (device_id or "").strip()
    email = (owner_email or "").strip()
    if not UUID_RE.fullmatch(did):
        raise FleetOpsError("设备编号必须是合法 UUID")
    if email and not EMAIL_RE.fullmatch(email):
        raise FleetOpsError("归属账号邮箱格式无效")

    jump = robot_ops._connect_jump(cfg, timeout_s=8)
    try:
        if email:
            find_sql = f"SELECT id::text FROM auth.users WHERE lower(email) = lower({_sql_literal(email)}) LIMIT 1;"
            c1, o1, _ = robot_ops._exec_cmd(
                jump,
                "sudo -n docker exec -i supabase-db psql -At -U postgres -d postgres",
                timeout_s=10,
                stdin_text=find_sql + "\n",
            )
            owner_id = o1.strip().splitlines()[0] if (c1 == 0 and o1.strip()) else ""
            if not owner_id:
                raise FleetOpsError(f"未找到账号: {email}")
            upd_sql = (
                f"UPDATE public.devices SET owner_id = {_sql_literal(owner_id)}::uuid "
                f"WHERE id = {_sql_literal(did)}::uuid RETURNING id::text;"
            )
        else:
            owner_id = ""
            upd_sql = (
                f"UPDATE public.devices SET owner_id = NULL "
                f"WHERE id = {_sql_literal(did)}::uuid RETURNING id::text;"
            )

        c2, o2, e2 = robot_ops._exec_cmd(
            jump,
            "sudo -n docker exec -i supabase-db psql -v ON_ERROR_STOP=1 -At -U postgres -d postgres",
            timeout_s=10,
            stdin_text=upd_sql + "\n",
        )
        if c2 != 0 or not o2.strip():
            raise FleetOpsError(f"更新设备归属失败（确认设备已在业务库存中）: {(e2 or o2).strip()}")
        return {"ok": True, "deviceId": did, "ownerId": owner_id, "ownerEmail": email}
    finally:
        jump.close()
