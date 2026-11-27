(() => {
  const STATE = {
    view: "devices",
    overview: null,
    localArtifacts: { apks: [], robotFiles: [] },
    pubSourceMode: "local_dir",
    pubUpload: null,
    pubReleaseIdTouched: false,
    apkSourceMode: "upload",
    apkUpload: null,
    apkLocalPath: "",
    pollTimer: null,
    editingDeviceId: null,
    opsMode: "tunnel",
    opsTab: "control",
    opsTunnels: [],
    opsQuickDirs: [],
    opsPath: "/opt/doso",
    opsConnected: false,
  };

  const VIEW_META = {
    devices: { title: "设备车队", cta: "发布固件" },
    releases: { title: "机器狗版本", cta: "发布固件" },
    rollouts: { title: "灰度发布", cta: "新建灰度" },
    android: { title: "App 更新", cta: null },
    ops: { title: "机载运维", cta: null },
    settings: { title: "云端配置", cta: null },
  };

  const ARTIFACT_LABELS = {
    brainstem_release: "核心固件",
    policy_package: "运动策略",
    component_release: "独立组件",
    release_bundle: "整机组合包",
  };

  const PHASE_MAP = {
    scheduled: { label: "已调度", pct: 15, cls: "badge-info" },
    downloading: { label: "下载中", pct: 38, cls: "badge-info" },
    waiting_safe_window: { label: "等待安全窗口", pct: 55, cls: "badge-warn" },
    installing: { label: "安装中", pct: 72, cls: "badge-info" },
    rebooting: { label: "重启中", pct: 85, cls: "badge-info" },
    health_checking: { label: "健康检查", pct: 94, cls: "badge-info" },
    succeeded: { label: "已完成", pct: 100, cls: "badge-ok" },
    failed: { label: "失败", pct: 100, cls: "badge-err" },
    rolled_back: { label: "已回滚", pct: 100, cls: "badge-err" },
    recovery_required: { label: "需人工恢复", pct: 100, cls: "badge-err" },
    cancelled: { label: "已取消", pct: 100, cls: "badge-neutral" },
  };

  const ROLLOUT_STATUS_MAP = {
    active: { label: "进行中", cls: "badge-info" },
    paused_manual: { label: "已暂停", cls: "badge-warn" },
    paused_circuit_open: { label: "熔断暂停", cls: "badge-err" },
    completed: { label: "已完成", cls: "badge-ok" },
    aborted: { label: "已中止", cls: "badge-neutral" },
  };

  const CANCELLABLE_PHASES = new Set(["scheduled", "downloading", "waiting_safe_window"]);
  const ACTIVE_PHASES = new Set([
    "scheduled",
    "downloading",
    "waiting_safe_window",
    "installing",
    "rebooting",
    "health_checking",
  ]);

  const $ = (id) => document.getElementById(id);

  function esc(str) {
    return String(str ?? "")
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function formatBytes(bytes) {
    const n = Number(bytes || 0);
    if (n <= 0) return "—";
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  function formatAge(ms) {
    if (ms === null || ms === undefined) return "—";
    const s = Math.floor(Number(ms) / 1000);
    if (s < 60) return `${s} 秒前`;
    if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
    if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
    return `${Math.floor(s / 86400)} 天前`;
  }

  function formatTime(ms) {
    if (!ms) return "—";
    const d = new Date(Number(ms));
    const pad = (v) => String(v).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function toast(message, kind = "info") {
    const stack = $("toast-stack");
    const el = document.createElement("div");
    el.className = `toast ${kind === "err" ? "toast-err" : kind === "ok" ? "toast-ok" : ""}`;
    el.textContent = message;
    stack.appendChild(el);
    setTimeout(() => el.remove(), kind === "err" ? 5500 : 3200);
  }

  async function apiFetch(path, options = {}) {
    const headers = Object.assign({ "X-DOSO-Console": "1" }, options.headers || {});
    if (options.json !== undefined) {
      headers["Content-Type"] = "application/json";
    }
    const resp = await fetch(path, {
      method: options.method || (options.json !== undefined ? "POST" : "GET"),
      headers,
      body: options.json !== undefined ? JSON.stringify(options.json) : options.body,
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok) {
      throw new Error(data.error || `请求失败 (${resp.status})`);
    }
    return data;
  }

  async function uploadRawFile(file, onMetaEl) {
    if (onMetaEl) onMetaEl.textContent = `正在上传 ${file.name} (${formatBytes(file.size)})…`;
    const resp = await fetch("/api/uploads", {
      method: "POST",
      headers: {
        "X-DOSO-Console": "1",
        "X-Upload-Filename": encodeURIComponent(file.name),
      },
      body: file,
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok) {
      throw new Error(data.error || `上传失败 (${resp.status})`);
    }
    return data;
  }

  function renderSteps(container, steps, extraText = "") {
    if (!steps || !steps.length) {
      container.classList.add("hidden");
      return;
    }
    const STEP_LABELS = {
      package: "打包校验",
      upload: "上传存储",
      register: "云端登记",
      dispatch: "下发任务",
      publish: "发布版本",
      verify: "接口校验",
    };
    const STATUS_BADGE = {
      ok: `<span class="badge badge-ok">完成</span>`,
      pending: `<span class="badge badge-warn">待完成</span>`,
      dry_run: `<span class="badge badge-info">预检</span>`,
      skipped: `<span class="badge badge-neutral">跳过</span>`,
    };
    const rows = steps
      .map(
        (s) =>
          `<div class="step-row">${STATUS_BADGE[s.status] || ""}<strong>${esc(
            STEP_LABELS[s.step] || s.step
          )}</strong><span class="sub-line">${esc(s.detail || "")}</span></div>`
      )
      .join("");
    container.innerHTML = rows + (extraText ? `<div class="sub-line" style="margin-top:6px">${esc(extraText)}</div>` : "");
    container.classList.remove("hidden");
  }

  function syncConfigBannerVisibility() {
    const banner = $("config-banner");
    const shouldShow = banner.dataset.active === "1" && STATE.view !== "settings";
    banner.classList.toggle("hidden", !shouldShow);
  }

  function setView(viewName) {
    STATE.view = viewName;
    document.querySelectorAll(".nav-item").forEach((btn) => {
      btn.classList.toggle("active", btn.dataset.view === viewName);
    });
    document.querySelectorAll(".view").forEach((sec) => {
      sec.classList.toggle("active", sec.id === `view-${viewName}`);
    });
    const meta = VIEW_META[viewName] || VIEW_META.devices;
    $("page-title").textContent = meta.title;
    const cta = $("btn-primary-cta");
    if (meta.cta) {
      cta.textContent = meta.cta;
      cta.classList.remove("hidden");
    } else {
      cta.classList.add("hidden");
    }
    syncConfigBannerVisibility();
    $("sidebar").classList.remove("open");
    if (viewName === "ops" && !STATE.opsTunnels.length) {
      scanOpsTunnels(true);
    }
  }

  function openDrawer(id) {
    $(id).classList.remove("hidden");
  }

  function closeDrawer(id) {
    $(id).classList.add("hidden");
  }

  function mergeFleetDevices(ov) {
    const statusMap = new Map();
    for (const st of ov?.devices || []) {
      statusMap.set(st.deviceId, st);
    }
    const merged = [];
    const seen = new Set();
    for (const reg of ov?.registeredDevices || []) {
      seen.add(reg.deviceId);
      const st = statusMap.get(reg.deviceId);
      merged.push({
        deviceId: reg.deviceId,
        model: reg.model,
        hardwareRevision: reg.hardwareRevision,
        ring: reg.ring || "prod",
        enabled: reg.enabled !== false,
        tags: reg.tags || [],
        registered: true,
        status: st || null,
      });
    }
    for (const st of ov?.devices || []) {
      if (!seen.has(st.deviceId)) {
        merged.push({
          deviceId: st.deviceId,
          model: "",
          hardwareRevision: "",
          ring: "prod",
          enabled: true,
          tags: [],
          registered: false,
          status: st,
        });
      }
    }
    return merged;
  }

  function renderConnectionStatus(ov) {
    const dot = $("conn-dot");
    const label = $("conn-label");
    const banner = $("config-banner");
    const bannerText = $("config-banner-text");

    if (!ov.cloudReachable) {
      dot.className = "dot dot-err";
      label.textContent = "云端离线";
      bannerText.textContent = ov.warnings?.[0] || "无法连接云端服务";
      banner.dataset.active = "1";
      syncConfigBannerVisibility();
      return;
    }
    if (!ov.config?.tokenConfigured) {
      dot.className = "dot dot-warn";
      label.textContent = "待配置令牌";
      bannerText.textContent = "尚未配置云端发布令牌";
      banner.dataset.active = "1";
      syncConfigBannerVisibility();
      return;
    }
    if (!ov.authOk) {
      dot.className = "dot dot-err";
      label.textContent = "鉴权失败";
      bannerText.textContent = ov.warnings?.[0] || "访问令牌无效";
      banner.dataset.active = "1";
      syncConfigBannerVisibility();
      return;
    }
    dot.className = "dot dot-ok";
    label.textContent = "云端在线";
    banner.dataset.active = "0";
    syncConfigBannerVisibility();
  }

  function renderMetrics(ov) {
    const fleet = mergeFleetDevices(ov);
    const releases = ov.releases || [];
    const rollouts = ov.rollouts || [];
    const activeRollouts = rollouts.filter((r) => r.status === "active").length;
    const prodApp = ov.android?.prod;

    $("stat-devices").textContent = String(fleet.length);
    $("stat-releases").textContent = String(releases.length);
    $("stat-rollouts").textContent = String(activeRollouts);
    $("stat-android").textContent = prodApp ? `v${prodApp.versionName} (${prodApp.versionCode})` : "未发布";

    $("nav-count-devices").textContent = String(fleet.length);
    $("nav-count-releases").textContent = String(releases.length);
    $("nav-count-rollouts").textContent = String(rollouts.length);
  }

  function renderTrackedJobs(jobs) {
    const block = $("active-jobs-block");
    const list = $("job-list");
    if (!jobs || !jobs.length) {
      block.classList.add("hidden");
      list.innerHTML = "";
      return;
    }
    block.classList.remove("hidden");
    list.innerHTML = jobs
      .map((j) => {
        const info = PHASE_MAP[j.phase] || { label: j.phase, pct: 20, cls: "badge-neutral" };
        const canCancel = CANCELLABLE_PHASES.has(j.phase);
        return `
          <div class="job-card">
            <div class="job-meta">
              <div class="job-title">
                <span class="mono">${esc(j.deviceId)}</span>
                <span class="badge ${info.cls}">${esc(info.label)}</span>
              </div>
              <div class="sub-line">版本 <span class="mono">${esc(j.releaseId)}</span>${
                j.bytesDownloaded ? ` · 已下载 ${formatBytes(j.bytesDownloaded)}` : ""
              }${j.message ? ` · ${esc(j.message)}` : ""}</div>
            </div>
            <div style="display:flex;align-items:center;gap:12px;">
              <div class="progress-track"><div class="progress-fill" style="width:${info.pct}%"></div></div>
              ${
                canCancel
                  ? `<button class="btn btn-sm btn-danger" data-cancel-job="${esc(j.jobId)}" data-cancel-dev="${esc(
                      j.deviceId
                    )}" type="button">取消</button>`
                  : ""
              }
            </div>
          </div>`;
      })
      .join("");
  }

  function renderDevices(ov) {
    const fleet = mergeFleetDevices(ov);
    const q = ($("device-search").value || "").trim().toLowerCase();
    const filtered = fleet.filter((d) => {
      if (!q) return true;
      return (
        String(d.deviceId || "").toLowerCase().includes(q) ||
        String(d.ring || "").toLowerCase().includes(q) ||
        String(d.status?.firmwareVersion || "").toLowerCase().includes(q) ||
        (d.tags || []).join(" ").toLowerCase().includes(q)
      );
    });

    const datalist = $("known-devices-list");
    datalist.innerHTML = fleet.map((d) => `<option value="${esc(d.deviceId)}"></option>`).join("");

    const tbody = $("devices-tbody");
    if (!filtered.length) {
      tbody.innerHTML = `<tr><td colspan="6" class="empty-cell">暂无已登记设备</td></tr>`;
      return;
    }

    tbody.innerHTML = filtered
      .map((d) => {
        const st = d.status;
        const online = st && st.lastSeenAgeMs !== null && st.lastSeenAgeMs !== undefined && st.lastSeenAgeMs < 120000;
        const ready = st && st.brainstemReady && st.strategyReady;
        const statusBadge = !d.enabled
          ? `<span class="badge badge-err">已停用</span>`
          : online
          ? ready
            ? `<span class="badge badge-ok"><span class="dot dot-ok"></span>在线就绪</span>`
            : `<span class="badge badge-warn"><span class="dot dot-warn"></span>在线未就绪</span>`
          : `<span class="badge badge-neutral"><span class="dot dot-neutral"></span>离线</span>`;
        const ringCls = d.ring === "prod" ? "badge-info" : d.ring === "canary" ? "badge-warn" : "badge-neutral";
        const tagsLine = (d.tags || []).length ? (d.tags || []).join(" · ") : d.model || "";
        return `
          <tr>
            <td>
              <div class="mono">${esc(d.deviceId)}</div>
              <div class="sub-line">${esc(tagsLine)}</div>
            </td>
            <td><span class="badge ${ringCls}">${esc(d.ring)}</span></td>
            <td>${statusBadge}</td>
            <td>
              <div>${esc(st?.firmwareVersion || "—")}</div>
              <div class="sub-line mono">${esc(st?.releaseId || "")}</div>
            </td>
            <td>${esc(formatAge(st?.lastSeenAgeMs))}</td>
            <td class="col-actions">
              <button class="btn btn-sm btn-ghost" data-edit-device="${esc(d.deviceId)}" type="button">编辑</button>
              <button class="btn btn-sm btn-ghost" data-push-device="${esc(d.deviceId)}" type="button">推送更新</button>
            </td>
          </tr>`;
      })
      .join("");
  }

  function renderReleases(releases) {
    const q = ($("release-search").value || "").trim().toLowerCase();
    const sorted = [...(releases || [])].sort((a, b) => Number(b.sequence || 0) - Number(a.sequence || 0));
    const filtered = sorted.filter((r) => {
      if (!q) return true;
      return (
        String(r.releaseId || "").toLowerCase().includes(q) ||
        String(r.version || "").toLowerCase().includes(q) ||
        String(r.channel || "").toLowerCase().includes(q)
      );
    });

    const tbody = $("releases-tbody");
    if (!filtered.length) {
      tbody.innerHTML = `<tr><td colspan="7" class="empty-cell">暂无已登记版本</td></tr>`;
      return;
    }

    tbody.innerHTML = filtered
      .map((r) => {
        const typeLabel = ARTIFACT_LABELS[r.artifactType] || r.artifactType || "核心固件";
        const extra = r.policySlot ? ` · ${r.policySlot}` : r.component ? ` · ${r.component}` : "";
        const chCls = r.channel === "stable" ? "badge-ok" : "badge-neutral";
        return `
          <tr>
            <td>
              <div class="mono">${esc(r.releaseId)}</div>
              <div class="sub-line mono">${esc((r.artifactSha256 || "").slice(0, 12))}</div>
            </td>
            <td>${esc(typeLabel + extra)}</td>
            <td><span class="badge ${chCls}">${esc(r.channel)}</span></td>
            <td>${esc(r.version)}</td>
            <td class="mono">${esc(r.sequence)}</td>
            <td>${esc(formatBytes(r.artifactSizeBytes))}</td>
            <td class="col-actions">
              <button class="btn btn-sm btn-ghost" data-push-release="${esc(r.releaseId)}" type="button">推送设备</button>
              <button class="btn btn-sm btn-ghost" data-rollout-release="${esc(r.releaseId)}" type="button">发起灰度</button>
            </td>
          </tr>`;
      })
      .join("");
  }

  function renderRollouts(rollouts) {
    const wrap = $("rollout-list");
    if (!rollouts || !rollouts.length) {
      wrap.innerHTML = `<div class="empty-cell">暂无灰度发布计划</div>`;
      return;
    }

    const sorted = [...rollouts].sort((a, b) => Number(b.createdAtMs || 0) - Number(a.createdAtMs || 0));
    wrap.innerHTML = sorted
      .map((r) => {
        const st = ROLLOUT_STATUS_MAP[r.status] || { label: r.status, cls: "badge-neutral" };
        const canPromote = r.status === "active";
        const canPause = r.status === "active";
        const canResume = r.status === "paused_manual" || r.status === "paused_circuit_open";
        const canAbort = r.status !== "completed" && r.status !== "aborted";
        const stagesHtml = (r.stageCounts || [])
          .map((s) => {
            const isCur = s.stageIndex === r.currentStageIndex && r.status === "active";
            return `
              <div class="stage-box ${isCur ? "current" : ""}">
                <div class="stage-name">${esc(s.name)} · ${esc(s.ring)} (${esc(s.percent)}%)</div>
                <div class="stage-stats">成功 ${s.succeeded} / 进行中 ${s.inProgress} / 失败 ${
              s.failed + s.rolledBack + s.recoveryRequired
            }</div>
              </div>`;
          })
          .join("");

        return `
          <div class="rollout-card">
            <div class="rollout-top">
              <div>
                <div class="rollout-title">
                  <span class="mono">${esc(r.rolloutId)}</span>
                  <span class="badge ${st.cls}">${esc(st.label)}</span>
                </div>
                <div class="sub-line">版本 <span class="mono">${esc(r.releaseId)}</span> · 机型 ${esc(
          r.targetModel || "—"
        )}${r.circuitOpenReason ? ` · 熔断原因 ${esc(r.circuitOpenReason)}` : ""}</div>
              </div>
              <div class="rollout-actions">
                ${
                  canPromote
                    ? `<button class="btn btn-sm btn-primary" data-ro-act="promote" data-ro-id="${esc(
                        r.rolloutId
                      )}" type="button">推进阶段</button>`
                    : ""
                }
                ${
                  canPause
                    ? `<button class="btn btn-sm btn-ghost" data-ro-act="pause" data-ro-id="${esc(
                        r.rolloutId
                      )}" type="button">暂停</button>`
                    : ""
                }
                ${
                  canResume
                    ? `<button class="btn btn-sm btn-primary" data-ro-act="resume" data-ro-id="${esc(
                        r.rolloutId
                      )}" type="button">恢复</button>`
                    : ""
                }
                ${
                  canAbort
                    ? `<button class="btn btn-sm btn-danger" data-ro-act="abort" data-ro-id="${esc(
                        r.rolloutId
                      )}" type="button">中止</button>`
                    : ""
                }
              </div>
            </div>
            <div class="stages-row">${stagesHtml}</div>
          </div>`;
      })
      .join("");
  }

  function renderAndroidChannels(androidMap) {
    const wrap = $("android-channels");
    const channels = ["prod", "qa", "beta"];
    wrap.innerHTML = channels
      .map((ch) => {
        const item = androidMap?.[ch];
        if (!item) {
          return `
            <div class="channel-card">
              <div class="channel-head">
                <span class="channel-name">${esc(ch)}</span>
                <span class="badge badge-neutral">未发布</span>
              </div>
              <div class="sub-line">暂无线上版本</div>
            </div>`;
        }
        return `
          <div class="channel-card">
            <div class="channel-head">
              <span class="channel-name">${esc(ch)}</span>
              ${
                item.mandatory
                  ? `<span class="badge badge-warn">强制更新</span>`
                  : `<span class="badge badge-ok">线上生效</span>`
              }
            </div>
            <div class="channel-ver">v${esc(item.versionName)} <span class="sub-line mono">(${esc(
          item.versionCode
        )})</span></div>
            <div class="sub-line">${esc(formatBytes(item.apkSizeBytes))} · ${esc(formatTime(item.publishedAtMs))}</div>
            ${item.changelog ? `<div class="channel-desc">${esc(item.changelog)}</div>` : ""}
          </div>`;
      })
      .join("");

    const selectedCh = $("apk-channel").value || "prod";
    const cur = androidMap?.[selectedCh];
    if (!$("apk-version-code").value) {
      $("apk-version-code").value = String(cur ? Number(cur.versionCode) + 1 : 57);
    }
    if (!$("apk-version-name").value) {
      $("apk-version-name").value = cur ? cur.versionName : "0.1.0";
    }
  }

  function populateSettingsForm(cfg) {
    if (!cfg) return;
    $("cfg-cloud-url").value = cfg.cloudUrl || "";
    $("cfg-oss-bucket").value = cfg.ossBucket || "";
    $("cfg-oss-endpoint").value = cfg.ossEndpoint || "";
    $("cfg-model").value = cfg.defaultModel || "";
    $("cfg-hw").value = cfg.defaultHardwareRevision || "";
    $("cfg-key-id").value = cfg.signingKeyId || "";
    $("cfg-token-status").textContent = cfg.tokenConfigured ? `已配置 ${cfg.tokenPreview}` : "未配置";
    $("cfg-akid-status").textContent = cfg.ossAkIdConfigured ? `已配置 ${cfg.ossAkIdPreview}` : "未配置";
    $("cfg-aksec-status").textContent = cfg.ossAkSecretConfigured ? `已配置 ${cfg.ossAkSecretPreview}` : "未配置";
    $("cfg-ops-host").value = cfg.opsJumpHost || "";
    $("cfg-ops-user").value = cfg.opsJumpUser || "";
    $("cfg-ops-key").value = cfg.opsJumpKey || "";
    $("cfg-ops-key-status").textContent = cfg.opsJumpKeyReady ? "已就绪" : "未检测到";
    $("cfg-ops-robot-user").value = cfg.opsRobotUser || "sunrise";
    $("cfg-ops-pwd-status").textContent = cfg.opsRobotPasswordConfigured ? "已配置" : "未配置";
    $("cfg-owner-email").value = cfg.defaultOwnerEmail || "";
    $("cfg-ecs-instance").value = cfg.ecsInstanceId || "";
    $("cfg-ecsak-status").textContent = cfg.ecsAkIdConfigured ? `已配置 ${cfg.ecsAkIdPreview}` : "未配置";
    $("cfg-ecssec-status").textContent = cfg.ecsAkSecretConfigured ? "已配置" : "未配置";
  }

  function renderLocalArtifactChips() {
    const apkWrap = $("local-apk-chips");
    const apks = STATE.localArtifacts.apks || [];
    apkWrap.innerHTML = apks
      .map(
        (a, idx) =>
          `<button type="button" class="chip-btn" data-local-apk-idx="${idx}">本机 ${esc(a.label)} · ${esc(
            formatBytes(a.sizeBytes)
          )}</button>`
      )
      .join("");

    const robotWrap = $("local-robot-chips");
    const rFiles = STATE.localArtifacts.robotFiles || [];
    robotWrap.innerHTML = rFiles
      .map(
        (f, idx) =>
          `<button type="button" class="chip-btn" data-local-robot-idx="${idx}">本机 ${esc(f.label)} · ${esc(
            formatBytes(f.sizeBytes)
          )}</button>`
      )
      .join("");
  }

  function scheduleJobPolling() {
    if (STATE.pollTimer) {
      clearTimeout(STATE.pollTimer);
      STATE.pollTimer = null;
    }
    const jobs = STATE.overview?.trackedJobs || [];
    const hasActive = jobs.some((j) => ACTIVE_PHASES.has(j.phase));
    if (hasActive) {
      STATE.pollTimer = setTimeout(() => refreshAll(true), 3000);
    }
  }

  async function refreshAll(silent = false) {
    const btn = $("btn-refresh");
    const icon = btn.querySelector("svg");
    if (!silent) icon.classList.add("spin");
    try {
      const ov = await apiFetch("/api/overview");
      STATE.overview = ov;
      renderConnectionStatus(ov);
      renderMetrics(ov);
      renderTrackedJobs(ov.trackedJobs);
      renderDevices(ov);
      renderReleases(ov.releases);
      renderRollouts(ov.rollouts);
      renderAndroidChannels(ov.android);
      populateSettingsForm(ov.config);
      scheduleJobPolling();
    } catch (err) {
      if (!silent) toast(err.message, "err");
    } finally {
      icon.classList.remove("spin");
    }
  }

  async function loadLocalArtifacts() {
    try {
      STATE.localArtifacts = await apiFetch("/api/local-artifacts");
      renderLocalArtifactChips();
    } catch {
      // Optional local convenience probe
    }
  }

  // Device Registry Drawer
  function openDeviceDrawer(existing = null) {
    STATE.editingDeviceId = existing ? existing.deviceId : null;
    const cfg = STATE.overview?.config;
    $("drawer-device-title").textContent = existing ? "编辑机器狗" : "登记机器狗";
    $("dev-id-input").value = existing ? existing.deviceId : "";
    $("dev-id-input").disabled = Boolean(existing);
    $("dev-model-input").value = existing?.model || cfg?.defaultModel || "THUNDER 01";
    $("dev-hw-input").value = existing?.hardwareRevision || cfg?.defaultHardwareRevision || "rdk-s100p";
    $("dev-ring-input").value = existing?.ring || "prod";
    $("dev-enabled-input").checked = existing ? existing.enabled !== false : true;
    $("dev-tags-input").value = (existing?.tags || []).join(", ");
    $("btn-dev-delete").classList.toggle("hidden", !existing);
    openDrawer("drawer-device");
  }

  // Bundle Composer Drawer
  function openBundleDrawer() {
    const releases = STATE.overview?.releases || [];
    const maxSeq = releases.reduce((acc, r) => Math.max(acc, Number(r.sequence || 0)), 0);
    const eligible = [...releases]
      .filter(
        (r) =>
          r.artifactType === "brainstem_release" ||
          (r.artifactType === "component_release" && r.component !== "ota")
      )
      .sort((a, b) => Number(b.sequence || 0) - Number(a.sequence || 0));

    const box = $("bundle-source-list");
    if (!eligible.length) {
      box.innerHTML = `<div class="sub-line">暂无可合成的组件版本</div>`;
    } else {
      box.innerHTML = eligible
        .map(
          (r) => `
          <label class="check-row">
            <input type="checkbox" value="${esc(r.releaseId)}" />
            <span class="mono">${esc(r.releaseId)}</span>
            <span class="badge badge-neutral">${esc(r.component || "brainstem")}</span>
            <span class="sub-line">v${esc(r.version)}</span>
          </label>`
        )
        .join("");
    }
    $("bnd-version").value = "1.0.0";
    $("bnd-sequence").value = String(maxSeq + 1);
    $("bnd-release-id").value = `bundle-1.0.0-${$("bnd-channel").value}-s${maxSeq + 1}`;
    openDrawer("drawer-bundle");
  }

  // Robot Remote Ops Helpers
  function currentOpsTarget() {
    const username = $("ops-user-override")?.value.trim() || undefined;
    const password = $("ops-pwd-override")?.value.trim() || undefined;
    if (STATE.opsMode === "direct") {
      return { mode: "direct", host: $("ops-direct-host").value.trim(), port: 22, username, password };
    }
    const port = Number($("ops-tunnel-select").value || 0);
    return { mode: "tunnel", port, username, password };
  }

  function setOpsMode(mode) {
    STATE.opsMode = mode;
    document.querySelectorAll("#ops-mode-tabs .seg-btn").forEach((b) => {
      b.classList.toggle("active", b.dataset.opsMode === mode);
    });
    $("ops-tunnel-picker").classList.toggle("hidden", mode !== "tunnel");
    $("ops-direct-picker").classList.toggle("hidden", mode !== "direct");
  }

  function setOpsTab(tab) {
    STATE.opsTab = tab;
    document.querySelectorAll("#ops-subtabs .subtab").forEach((b) => {
      b.classList.toggle("active", b.dataset.opsTab === tab);
    });
    document.querySelectorAll(".ops-pane").forEach((p) => {
      p.classList.toggle("active", p.id === `ops-pane-${tab}`);
    });
  }

  async function scanOpsTunnels(silent = false) {
    try {
      const data = await apiFetch("/api/ops/tunnels");
      STATE.opsTunnels = data.tunnels || [];
      STATE.opsQuickDirs = data.quickDirs || [];
      const sel = $("ops-tunnel-select");
      if (!STATE.opsTunnels.length) {
        sel.innerHTML = `<option value="">暂无在线隧道</option>`;
      } else {
        sel.innerHTML = STATE.opsTunnels
          .map((t) => `<option value="${esc(t.port)}">${esc(t.label)}</option>`)
          .join("");
      }
      $("ops-quick-dirs").innerHTML = STATE.opsQuickDirs
        .map((d) => `<button type="button" class="chip-btn" data-quick-dir="${esc(d.path)}">${esc(d.label)}</button>`)
        .join("");
      if (!silent) toast(`发现 ${STATE.opsTunnels.length} 台在线隧道设备`, "ok");
    } catch (err) {
      if (!silent) toast(err.message, "err");
    }
  }

  async function loadOpsStatus() {
    const data = await apiFetch("/api/ops/status", { json: { target: currentOpsTarget() } });
    STATE.opsConnected = true;
    const h = data.health || {};
    $("ops-st-ready").textContent = h.brainstem_ready ? (h.cms_state || "就绪") : "未就绪";
    $("ops-st-motor").textContent = h.motor_output_enabled ? "已使能" : "已关闭";
    $("ops-st-vbus").textContent = typeof h.vbus_min_v === "number" ? `${h.vbus_min_v.toFixed(1)} V` : "—";
    $("ops-st-disk").textContent = data.disk ? `${data.disk.usePercent} (${formatBytes(data.disk.availBytes)} 可用)` : "—";
    $("ops-host-meta").textContent = `${data.hostname || ""} · ${data.uptime || ""}`;

    const tbody = $("ops-services-tbody");
    tbody.innerHTML = (data.services || [])
      .map((s) => {
        const active = s.state === "active";
        const badge = active
          ? `<span class="badge badge-ok">运行中</span>`
          : `<span class="badge badge-neutral">${esc(s.state)}</span>`;
        return `
          <tr>
            <td class="mono">${esc(s.unit)}</td>
            <td>${badge}</td>
            <td class="col-actions">
              <button class="btn btn-sm btn-ghost" data-svc-unit="${esc(s.unit)}" data-svc-act="restart" type="button">重启</button>
              <button class="btn btn-sm btn-ghost" data-svc-unit="${esc(s.unit)}" data-svc-act="${active ? "stop" : "start"}" type="button">${active ? "停止" : "启动"}</button>
            </td>
          </tr>`;
      })
      .join("");
  }

  async function loadOpsFiles(pathStr) {
    const path = pathStr || $("fs-path-input").value.trim() || "/opt/doso";
    const data = await apiFetch("/api/ops/files/list", { json: { target: currentOpsTarget(), path } });
    STATE.opsPath = data.path;
    $("fs-path-input").value = data.path;
    $("btn-fs-up").disabled = !data.parent;
    $("btn-fs-up").dataset.parent = data.parent || "";

    const tbody = $("ops-files-tbody");
    if (!data.entries.length) {
      tbody.innerHTML = `<tr><td colspan="5" class="empty-cell">空目录</td></tr>`;
      return;
    }
    tbody.innerHTML = data.entries
      .map((e) => {
        const nameCell = e.isDir
          ? `<button type="button" class="dir-link" data-fs-open-dir="${esc(e.path)}">${esc(e.name)}/</button>`
          : `<span class="mono">${esc(e.name)}</span>`;
        const actions = e.isDir
          ? `<button class="btn btn-sm btn-danger" data-fs-del="${esc(e.path)}" type="button">删除</button>`
          : `<button class="btn btn-sm btn-ghost" data-fs-edit="${esc(e.path)}" type="button">编辑</button>
             <button class="btn btn-sm btn-ghost" data-fs-dl="${esc(e.path)}" type="button">下载</button>
             <button class="btn btn-sm btn-danger" data-fs-del="${esc(e.path)}" type="button">删除</button>`;
        return `
          <tr>
            <td>${nameCell}</td>
            <td>${e.isDir ? "—" : esc(formatBytes(e.sizeBytes))}</td>
            <td class="mono">${esc(e.mode)}</td>
            <td>${esc(formatTime(e.modifiedAtMs))}</td>
            <td class="col-actions">${actions}</td>
          </tr>`;
      })
      .join("");
  }

  async function connectOpsRobot() {
    const btn = $("btn-ops-connect");
    btn.disabled = true;
    try {
      await loadOpsStatus();
      await loadOpsFiles(STATE.opsPath);
      toast("已连接机器狗", "ok");
    } catch (err) {
      toast(err.message, "err");
    } finally {
      btn.disabled = false;
    }
  }

  // Publish Robot Drawer helpers
  function suggestNextReleaseMeta() {
    const releases = STATE.overview?.releases || [];
    const maxSeq = releases.reduce((acc, r) => Math.max(acc, Number(r.sequence || 0)), 0);
    $("pub-sequence").value = String(maxSeq + 1);
    if (!$("pub-version").value) $("pub-version").value = "0.2.0";
    const cfg = STATE.overview?.config;
    if (cfg) {
      $("pub-model").value = cfg.defaultModel || "THUNDER 01";
      $("pub-hw").value = cfg.defaultHardwareRevision || "rdk-s100p";
    }
    STATE.pubReleaseIdTouched = false;
    updateAutoReleaseId();
  }

  function updateAutoReleaseId() {
    if (STATE.pubReleaseIdTouched) return;
    const type = $("pub-type").value;
    const prefix = type === "policy_package" ? "policy" : type === "component_release" ? "comp" : "brainstem";
    const ver = ($("pub-version").value || "0.1.0").trim().replace(/^v/, "");
    const ch = $("pub-channel").value || "qa";
    const seq = $("pub-sequence").value || "1";
    $("pub-release-id").value = `${prefix}-${ver}-${ch}-s${seq}`;
  }

  function setPubSourceMode(mode) {
    STATE.pubSourceMode = mode;
    document.querySelectorAll("#pub-source-tabs .seg-btn").forEach((b) => {
      b.classList.toggle("active", b.dataset.source === mode);
    });
    $("pane-local-dir").classList.toggle("hidden", mode !== "local_dir");
    $("pane-upload").classList.toggle("hidden", mode !== "upload");
    $("pane-local-file").classList.toggle("hidden", mode !== "local_file");
  }

  async function inspectAndShowPath(pathStr, metaEl) {
    if (!pathStr.trim()) {
      metaEl.textContent = "";
      return;
    }
    try {
      const info = await apiFetch("/api/inspect-path", { json: { path: pathStr } });
      if (!info.exists) {
        metaEl.textContent = "路径不存在";
        return;
      }
      if (info.isDir) {
        const tag = info.hasBrainstemBin
          ? "检测到核心固件"
          : info.hasPolicyYaml
          ? "检测到运动策略"
          : `包含 ${info.fileCount} 个文件`;
        metaEl.textContent = `${tag} · 共 ${info.fileCount} 个文件`;
        if (info.hasBrainstemBin) $("pub-type").value = "brainstem_release";
        else if (info.hasPolicyYaml) $("pub-type").value = "policy_package";
        syncPubTypeFields();
        updateAutoReleaseId();
      } else {
        metaEl.textContent = `${info.name} · ${formatBytes(info.sizeBytes)} · SHA-256 ${info.sha256.slice(0, 12)}`;
      }
    } catch (err) {
      metaEl.textContent = err.message;
    }
  }

  function syncPubTypeFields() {
    const t = $("pub-type").value;
    $("pub-policy-slot-wrap").classList.toggle("hidden", t !== "policy_package");
    $("pub-component-wrap").classList.toggle("hidden", t !== "component_release");
  }

  function syncPubDispatchFields() {
    const m = $("pub-dispatch-mode").value;
    $("pub-device-wrap").classList.toggle("hidden", m !== "device");
    $("pub-rollout-wrap").classList.toggle("hidden", m !== "rollout");
  }

  async function submitRobotPublish(dryRun) {
    const payload = {
      artifactType: $("pub-type").value,
      channel: $("pub-channel").value,
      version: $("pub-version").value.trim(),
      sequence: Number($("pub-sequence").value),
      releaseId: $("pub-release-id").value.trim(),
      sourceMode: STATE.pubSourceMode,
      localPath: STATE.pubSourceMode === "local_dir" ? $("pub-dir-path").value.trim() : $("pub-file-path").value.trim(),
      uploadId: STATE.pubUpload?.uploadId || "",
      policySlot: $("pub-policy-slot").value.trim(),
      component: $("pub-component").value.trim(),
      dispatchMode: $("pub-dispatch-mode").value,
      deviceId: $("pub-device-id").value.trim(),
      rolloutId: $("pub-rollout-id").value.trim(),
      model: $("pub-model").value.trim(),
      hardwareRevision: $("pub-hw").value.trim(),
      minBattery: Number($("pub-min-battery").value || 20),
      objectKey: $("pub-object-key").value.trim(),
      overwrite: $("pub-overwrite").checked,
      dryRun,
    };

    const btnSub = $("btn-pub-submit");
    const btnDry = $("btn-pub-dryrun");
    const resultEl = $("robot-publish-result");
    btnSub.disabled = true;
    btnDry.disabled = true;
    resultEl.innerHTML = `<div>正在执行发布流程…</div>`;
    resultEl.classList.remove("hidden");
    try {
      const res = await apiFetch("/api/releases/publish", { json: payload });
      renderSteps(resultEl, res.steps, dryRun ? "预检通过，未修改云端状态" : `版本 ${res.releaseId} 已就绪`);
      toast(dryRun ? "预检通过" : `版本 ${res.releaseId} 发布成功`, "ok");
      if (!dryRun) {
        await refreshAll(true);
      }
    } catch (err) {
      resultEl.innerHTML = `<div style="color:var(--err-text)">${esc(err.message)}</div>`;
      toast(err.message, "err");
    } finally {
      btnSub.disabled = false;
      btnDry.disabled = false;
    }
  }

  async function openPushDrawer({ deviceId = "", releaseId = "" } = {}) {
    $("push-device-id").value = deviceId;
    const select = $("push-release-id");
    let items = STATE.overview?.releases || [];
    if (deviceId) {
      try {
        const compat = await apiFetch(`/api/devices/${encodeURIComponent(deviceId)}/releases`);
        if (Array.isArray(compat) && compat.length) items = compat;
      } catch {
        // Fallback to global catalog
      }
    }
    const sorted = [...items].sort((a, b) => Number(b.sequence || 0) - Number(a.sequence || 0));
    if (!sorted.length) {
      select.innerHTML = `<option value="">暂无可推送版本</option>`;
    } else {
      select.innerHTML = sorted
        .map(
          (r) =>
            `<option value="${esc(r.releaseId)}" ${r.releaseId === releaseId ? "selected" : ""}>${esc(
              r.releaseId
            )} · v${esc(r.version)} (${esc(r.channel)})</option>`
        )
        .join("");
    }
    openDrawer("drawer-push");
  }

  function openRolloutDrawer(releaseId = "") {
    const items = [...(STATE.overview?.releases || [])].sort((a, b) => Number(b.sequence || 0) - Number(a.sequence || 0));
    const select = $("ro-release-id");
    if (!items.length) {
      select.innerHTML = `<option value="">暂无可用版本</option>`;
    } else {
      select.innerHTML = items
        .map(
          (r) =>
            `<option value="${esc(r.releaseId)}" ${r.releaseId === releaseId ? "selected" : ""}>${esc(
              r.releaseId
            )} · v${esc(r.version)}</option>`
        )
        .join("");
    }
    $("ro-id").value = "";
    openDrawer("drawer-rollout");
  }

  async function submitAndroidPublish(dryRun) {
    const payload = {
      sourceMode: STATE.apkSourceMode,
      uploadId: STATE.apkUpload?.uploadId || "",
      localPath: STATE.apkLocalPath,
      channel: $("apk-channel").value,
      versionName: $("apk-version-name").value.trim(),
      versionCode: Number($("apk-version-code").value),
      minSupportedCode: Number($("apk-min-code").value || 0),
      mandatory: $("apk-mandatory").checked,
      changelog: $("apk-changelog").value.trim(),
      objectKey: $("apk-object-key").value.trim(),
      overwrite: $("apk-overwrite").checked,
      dryRun,
    };

    const btnSub = $("btn-apk-submit");
    const btnDry = $("btn-apk-dryrun");
    const resultEl = $("apk-publish-result");
    btnSub.disabled = true;
    btnDry.disabled = true;
    resultEl.innerHTML = `<div>正在上传并发布…</div>`;
    resultEl.classList.remove("hidden");
    try {
      const res = await apiFetch("/api/android/publish", { json: payload });
      renderSteps(resultEl, res.steps, dryRun ? "预检通过，未上传云端" : "客户端现在即可检测到新版本");
      toast(dryRun ? "APK 预检通过" : "App 新版本已上线", "ok");
      if (!dryRun) await refreshAll(true);
    } catch (err) {
      resultEl.innerHTML = `<div style="color:var(--err-text)">${esc(err.message)}</div>`;
      toast(err.message, "err");
    } finally {
      btnSub.disabled = false;
      btnDry.disabled = false;
    }
  }

  function setupDropzone(zoneId, fileInputId, onFile) {
    const zone = $(zoneId);
    const input = $(fileInputId);
    zone.addEventListener("dragover", (e) => {
      e.preventDefault();
      zone.classList.add("dragover");
    });
    zone.addEventListener("dragleave", () => zone.classList.remove("dragover"));
    zone.addEventListener("drop", (e) => {
      e.preventDefault();
      zone.classList.remove("dragover");
      const file = e.dataTransfer?.files?.[0];
      if (file) onFile(file);
    });
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      if (file) onFile(file);
      input.value = "";
    });
  }

  function bindEvents() {
    document.querySelectorAll(".nav-item").forEach((btn) => {
      btn.addEventListener("click", () => setView(btn.dataset.view));
    });
    $("mobile-menu-btn").addEventListener("click", () => $("sidebar").classList.toggle("open"));
    $("conn-pill").addEventListener("click", () => setView("settings"));
    $("btn-goto-settings").addEventListener("click", () => setView("settings"));
    $("btn-refresh").addEventListener("click", () => refreshAll(false));

    $("btn-primary-cta").addEventListener("click", () => {
      if (STATE.view === "rollouts") {
        openRolloutDrawer();
      } else {
        suggestNextReleaseMeta();
        $("robot-publish-result").classList.add("hidden");
        openDrawer("drawer-publish");
      }
    });

    document.querySelectorAll("[data-close-drawer]").forEach((btn) => {
      btn.addEventListener("click", () => closeDrawer(btn.dataset.closeDrawer));
    });
    document.querySelectorAll(".drawer-backdrop").forEach((bg) => {
      bg.addEventListener("click", (e) => {
        if (e.target === bg) bg.classList.add("hidden");
      });
    });

    // Devices & Registry
    $("device-search").addEventListener("input", () => renderDevices(STATE.overview));
    $("release-search").addEventListener("input", () => renderReleases(STATE.overview?.releases));
    $("btn-manual-push").addEventListener("click", () => openPushDrawer());
    $("btn-add-device").addEventListener("click", () => openDeviceDrawer(null));
    $("btn-open-provision").addEventListener("click", () => openProvisionDrawer());
    $("btn-open-bundle").addEventListener("click", () => openBundleDrawer());

    // One-Click Provision Drawer Events
    let lastProvisionResult = null;
    function openProvisionDrawer() {
      const cfg = STATE.overview?.config || {};
      $("prov-model").value = cfg.defaultModel || "THUNDER 01";
      $("prov-owner").value = cfg.defaultOwnerEmail || "";
      $("prov-result-box").classList.add("hidden");
      $("prov-ecs-manual-box").classList.add("hidden");
      $("prov-ecs-bundle").value = "";
      $("prov-probe-summary").textContent = "输入地址后先探测硬件与空闲编号";
      lastProvisionResult = null;
      openDrawer("drawer-provision");
    }

    $("prov-num").addEventListener("input", () => {
      const n = Number($("prov-num").value || 0);
      if (n >= 1 && n <= 999) {
        const pad = String(n).padStart(3, "0");
        $("prov-serial").value = `THUNDER-DEV-${pad}`;
        $("prov-tunnel-port").value = String(22000 + n);
      }
    });

    $("btn-prov-probe").addEventListener("click", async () => {
      const btn = $("btn-prov-probe");
      const summary = $("prov-probe-summary");
      btn.disabled = true;
      summary.textContent = "正在探测机器狗与云端空闲编号…";
      try {
        const res = await apiFetch("/api/provision/probe", {
          json: {
            host: $("prov-host").value.trim(),
            username: $("prov-user").value.trim(),
            password: $("prov-pwd").value,
          },
        });
        $("prov-hw").value = res.hardwareRevision || "";
        $("prov-model").value = res.model || "THUNDER 01";
        $("prov-num").value = String(res.suggestedNumber || 4);
        $("prov-serial").value = res.suggestedSerial || "";
        $("prov-tunnel-port").value = String(res.suggestedTunnelPort || 22004);
        $("prov-device-id").value = res.existingValidUuid || res.suggestedUuid || "";
        summary.textContent = res.alreadyProvisioned
          ? `已存在身份 ${res.existingValidUuid} · ${res.hardwareRevision}`
          : `${res.hostname} · ${res.hardwareRevision} · ${res.brainstemVersion || "就绪"}`;
        toast("探测成功，已自动填充推荐参数", "ok");
      } catch (err) {
        summary.textContent = err.message;
        toast(err.message, "err");
      } finally {
        btn.disabled = false;
      }
    });

    $("provision-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      const btn = $("btn-prov-submit");
      const box = $("prov-result-box");
      const ecsBox = $("prov-ecs-manual-box");
      btn.disabled = true;
      ecsBox.classList.add("hidden");
      box.innerHTML = "<div>正在执行签发入网，请稍候…</div>";
      box.classList.remove("hidden");
      try {
        const num = Number($("prov-num").value || 4);
        const hw = $("prov-hw").value.trim();
        const res = await apiFetch("/api/provision/run", {
          json: {
            host: $("prov-host").value.trim(),
            username: $("prov-user").value.trim(),
            password: $("prov-pwd").value,
            robotNumber: num,
            serialNumber: $("prov-serial").value.trim(),
            tunnelPort: Number($("prov-tunnel-port").value || 22000 + num),
            model: $("prov-model").value.trim(),
            hardwareRevision: hw,
            ring: $("prov-ring").value,
            ownerEmail: $("prov-owner").value.trim(),
            deviceId: $("prov-device-id").value.trim(),
            tags: [`dog${num}`, hw].filter(Boolean),
            enableInventory: $("prov-opt-inv").checked,
            enableOta: $("prov-opt-ota").checked,
            enableTunnel: $("prov-opt-tunnel").checked,
            enableCloudLinkKeys: $("prov-opt-mqtt").checked,
          },
        });
        lastProvisionResult = res;
        $("prov-device-id").value = res.deviceId || "";
        const tail =
          res.ecsStatus === "manual_required"
            ? `设备 ${res.deviceId} 已完成 OTA 与隧道入网，还差一步 ECS 证书签发`
            : `设备 ${res.deviceId} 已全部签发入网`;
        renderSteps(box, res.steps, tail);
        if (res.ecsStatus === "manual_required" && res.ecsScript) {
          ecsBox.classList.remove("hidden");
        }
        toast("签发流程已执行完成", "ok");
        await refreshAll(true);
      } catch (err) {
        box.innerHTML = `<div style="color:var(--err-text)">${esc(err.message)}</div>`;
        toast(err.message, "err");
      } finally {
        btn.disabled = false;
      }
    });

    $("btn-prov-copy-ecs").addEventListener("click", async () => {
      if (!lastProvisionResult?.ecsScript) return;
      try {
        await navigator.clipboard.writeText(lastProvisionResult.ecsScript);
        toast("已复制 ECS 签发脚本", "ok");
      } catch {
        toast("复制失败，请手动选中复制", "err");
      }
    });

    $("btn-prov-finish-mqtt").addEventListener("click", async () => {
      const btn = $("btn-prov-finish-mqtt");
      const bundleText = $("prov-ecs-bundle").value.trim();
      if (!bundleText) {
        toast("请先粘贴 ECS 签发回执", "err");
        return;
      }
      btn.disabled = true;
      try {
        const res = await apiFetch("/api/provision/finish-mqtt", {
          json: {
            host: $("prov-host").value.trim(),
            username: $("prov-user").value.trim(),
            password: $("prov-pwd").value,
            deviceId: $("prov-device-id").value.trim(),
            bundleText,
            enableService: true,
          },
        });
        toast(`连云证书已安装 (${res.serviceState})`, "ok");
        $("prov-ecs-manual-box").classList.add("hidden");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      } finally {
        btn.disabled = false;
      }
    });

    $("devices-tbody").addEventListener("click", (e) => {
      const pushBtn = e.target.closest("[data-push-device]");
      if (pushBtn) {
        openPushDrawer({ deviceId: pushBtn.dataset.pushDevice });
        return;
      }
      const editBtn = e.target.closest("[data-edit-device]");
      if (editBtn) {
        const fleet = mergeFleetDevices(STATE.overview);
        const found = fleet.find((d) => d.deviceId === editBtn.dataset.editDevice);
        if (found) openDeviceDrawer(found);
      }
    });

    $("device-upsert-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      try {
        const saved = await apiFetch("/api/devices/upsert", {
          json: {
            deviceId: STATE.editingDeviceId || $("dev-id-input").value.trim(),
            model: $("dev-model-input").value.trim(),
            hardwareRevision: $("dev-hw-input").value.trim(),
            ring: $("dev-ring-input").value,
            enabled: $("dev-enabled-input").checked,
            tags: $("dev-tags-input").value,
          },
        });
        toast(`设备 ${saved.deviceId} 已保存`, "ok");
        closeDrawer("drawer-device");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("btn-dev-delete").addEventListener("click", async () => {
      if (!STATE.editingDeviceId) return;
      if (!window.confirm(`确认移除设备 ${STATE.editingDeviceId}？`)) return;
      try {
        await apiFetch(`/api/devices/${encodeURIComponent(STATE.editingDeviceId)}/delete`, { json: {} });
        toast("设备已移除", "ok");
        closeDrawer("drawer-device");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    // Bundle Composer
    $("bundle-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      const checked = Array.from(document.querySelectorAll("#bundle-source-list input[type=checkbox]:checked")).map(
        (el) => el.value
      );
      if (!checked.length) {
        toast("请至少勾选 1 个组件版本", "err");
        return;
      }
      try {
        const rel = await apiFetch("/api/releases/bundle", {
          json: {
            releaseId: $("bnd-release-id").value.trim(),
            version: $("bnd-version").value.trim(),
            sequence: Number($("bnd-sequence").value),
            channel: $("bnd-channel").value,
            sourceReleaseIds: checked,
          },
        });
        toast(`整机包 ${rel.releaseId} 已生成`, "ok");
        closeDrawer("drawer-bundle");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    // Releases & Jobs & Rollouts
    $("releases-tbody").addEventListener("click", (e) => {
      const pushBtn = e.target.closest("[data-push-release]");
      if (pushBtn) {
        openPushDrawer({ releaseId: pushBtn.dataset.pushRelease });
        return;
      }
      const roBtn = e.target.closest("[data-rollout-release]");
      if (roBtn) openRolloutDrawer(roBtn.dataset.rolloutRelease);
    });

    $("job-list").addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-cancel-job]");
      if (!btn) return;
      btn.disabled = true;
      try {
        await apiFetch(
          `/api/jobs/${encodeURIComponent(btn.dataset.cancelDev)}/${encodeURIComponent(btn.dataset.cancelJob)}/cancel`,
          { method: "POST", json: {} }
        );
        toast("已取消任务", "ok");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
        btn.disabled = false;
      }
    });

    $("rollout-list").addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-ro-act]");
      if (!btn) return;
      btn.disabled = true;
      try {
        await apiFetch(`/api/rollouts/${encodeURIComponent(btn.dataset.roId)}/${btn.dataset.roAct}`, {
          method: "POST",
          json: {},
        });
        toast("灰度状态已更新", "ok");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
        btn.disabled = false;
      }
    });

    // Robot Remote Ops Events
    document.querySelectorAll("#ops-mode-tabs .seg-btn").forEach((b) => {
      b.addEventListener("click", () => setOpsMode(b.dataset.opsMode));
    });
    document.querySelectorAll("#ops-subtabs .subtab").forEach((b) => {
      b.addEventListener("click", () => setOpsTab(b.dataset.opsTab));
    });
    $("btn-ops-scan").addEventListener("click", () => scanOpsTunnels(false));
    $("btn-ops-connect").addEventListener("click", () => connectOpsRobot());

    $("ops-services-tbody").addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-svc-unit]");
      if (!btn) return;
      const unit = btn.dataset.svcUnit;
      const action = btn.dataset.svcAct;
      btn.disabled = true;
      try {
        const res = await apiFetch("/api/ops/service", {
          json: { target: currentOpsTarget(), unit, action },
        });
        toast(`${res.unit} 已执行 ${res.action} (${res.state})`, "ok");
        await loadOpsStatus();
      } catch (err) {
        toast(err.message, "err");
        btn.disabled = false;
      }
    });

    $("ops-quick-dirs").addEventListener("click", (e) => {
      const btn = e.target.closest("[data-quick-dir]");
      if (!btn) return;
      loadOpsFiles(btn.dataset.quickDir).catch((err) => toast(err.message, "err"));
    });
    $("btn-fs-up").addEventListener("click", () => {
      const p = $("btn-fs-up").dataset.parent;
      if (p) loadOpsFiles(p).catch((err) => toast(err.message, "err"));
    });
    $("btn-fs-go").addEventListener("click", () => {
      loadOpsFiles($("fs-path-input").value.trim()).catch((err) => toast(err.message, "err"));
    });
    $("fs-path-input").addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        loadOpsFiles($("fs-path-input").value.trim()).catch((err) => toast(err.message, "err"));
      }
    });
    $("btn-fs-mkdir").addEventListener("click", async () => {
      const name = window.prompt("输入新目录名");
      if (!name || !name.trim()) return;
      const base = STATE.opsPath.replace(/\/+$/, "");
      try {
        await apiFetch("/api/ops/files/mkdir", {
          json: { target: currentOpsTarget(), path: `${base}/${name.trim()}` },
        });
        toast("目录已创建", "ok");
        await loadOpsFiles(STATE.opsPath);
      } catch (err) {
        toast(err.message, "err");
      }
    });
    $("btn-fs-upload").addEventListener("click", () => $("fs-upload-input").click());
    $("fs-upload-input").addEventListener("change", async () => {
      const file = $("fs-upload-input").files?.[0];
      $("fs-upload-input").value = "";
      if (!file) return;
      try {
        toast(`正在上传 ${file.name}…`);
        const rec = await uploadRawFile(file, null);
        await apiFetch("/api/ops/files/upload", {
          json: { target: currentOpsTarget(), remoteDir: STATE.opsPath, uploadId: rec.uploadId, filename: file.name },
        });
        toast(`${file.name} 已上传至机器狗`, "ok");
        await loadOpsFiles(STATE.opsPath);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("ops-files-tbody").addEventListener("click", async (e) => {
      const openDir = e.target.closest("[data-fs-open-dir]");
      if (openDir) {
        loadOpsFiles(openDir.dataset.fsOpenDir).catch((err) => toast(err.message, "err"));
        return;
      }
      const editBtn = e.target.closest("[data-fs-edit]");
      if (editBtn) {
        try {
          const res = await apiFetch("/api/ops/files/read", {
            json: { target: currentOpsTarget(), path: editBtn.dataset.fsEdit },
          });
          $("drawer-file-title").textContent = res.path;
          $("file-edit-path").value = res.path;
          $("file-edit-content").value = res.content;
          openDrawer("drawer-file-edit");
        } catch (err) {
          toast(err.message, "err");
        }
        return;
      }
      const dlBtn = e.target.closest("[data-fs-dl]");
      if (dlBtn) {
        try {
          const path = dlBtn.dataset.fsDl;
          const resp = await fetch("/api/ops/files/download", {
            method: "POST",
            headers: { "X-DOSO-Console": "1", "Content-Type": "application/json" },
            body: JSON.stringify({ target: currentOpsTarget(), path }),
          });
          if (!resp.ok) {
            const errObj = await resp.json().catch(() => ({}));
            throw new Error(errObj.error || `下载失败 (${resp.status})`);
          }
          const blob = await resp.blob();
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = path.split("/").pop() || "file";
          document.body.appendChild(a);
          a.click();
          a.remove();
          URL.revokeObjectURL(url);
        } catch (err) {
          toast(err.message, "err");
        }
        return;
      }
      const delBtn = e.target.closest("[data-fs-del]");
      if (delBtn) {
        const p = delBtn.dataset.fsDel;
        if (!window.confirm(`确认删除 ${p}？`)) return;
        try {
          await apiFetch("/api/ops/files/delete", { json: { target: currentOpsTarget(), path: p } });
          toast("已删除", "ok");
          await loadOpsFiles(STATE.opsPath);
        } catch (err) {
          toast(err.message, "err");
        }
      }
    });

    $("file-edit-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      try {
        await apiFetch("/api/ops/files/write", {
          json: {
            target: currentOpsTarget(),
            path: $("file-edit-path").value,
            content: $("file-edit-content").value,
          },
        });
        toast("文件已保存", "ok");
        closeDrawer("drawer-file-edit");
        await loadOpsFiles(STATE.opsPath);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("btn-ops-logs").addEventListener("click", async () => {
      const box = $("ops-log-output");
      box.textContent = "正在拉取日志…";
      try {
        const res = await apiFetch("/api/ops/logs", {
          json: {
            target: currentOpsTarget(),
            unit: $("ops-log-unit").value,
            lines: Number($("ops-log-lines").value || 100),
            keyword: $("ops-log-keyword").value.trim(),
          },
        });
        box.textContent = res.text || "无匹配日志";
        box.scrollTop = box.scrollHeight;
      } catch (err) {
        box.textContent = err.message;
        toast(err.message, "err");
      }
    });

    // Robot Publish Form
    document.querySelectorAll("#pub-source-tabs .seg-btn").forEach((btn) => {
      btn.addEventListener("click", () => setPubSourceMode(btn.dataset.source));
    });
    $("pub-type").addEventListener("change", () => {
      syncPubTypeFields();
      updateAutoReleaseId();
    });
    $("pub-channel").addEventListener("change", updateAutoReleaseId);
    $("pub-version").addEventListener("input", updateAutoReleaseId);
    $("pub-sequence").addEventListener("input", updateAutoReleaseId);
    $("pub-release-id").addEventListener("input", () => {
      STATE.pubReleaseIdTouched = true;
    });
    $("pub-dispatch-mode").addEventListener("change", syncPubDispatchFields);

    $("pub-dir-path").addEventListener("change", () => inspectAndShowPath($("pub-dir-path").value, $("pub-dir-meta")));
    $("pub-file-path").addEventListener("change", () => inspectAndShowPath($("pub-file-path").value, $("pub-file-meta")));

    $("btn-pick-dir").addEventListener("click", async () => {
      try {
        const res = await apiFetch("/api/pick-path", { json: { kind: "dir" } });
        if (!res.supported) {
          toast("当前环境不支持本机弹窗，请改用上传模式", "err");
          return;
        }
        if (res.path) {
          $("pub-dir-path").value = res.path;
          await inspectAndShowPath(res.path, $("pub-dir-meta"));
        }
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("btn-pick-file").addEventListener("click", async () => {
      try {
        const res = await apiFetch("/api/pick-path", { json: { kind: "archive" } });
        if (!res.supported) {
          toast("当前环境不支持本机弹窗，请改用上传模式", "err");
          return;
        }
        if (res.path) {
          $("pub-file-path").value = res.path;
          await inspectAndShowPath(res.path, $("pub-file-meta"));
        }
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("local-robot-chips").addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-local-robot-idx]");
      if (!btn) return;
      const item = STATE.localArtifacts.robotFiles[Number(btn.dataset.localRobotIdx)];
      if (!item) return;
      setPubSourceMode("local_file");
      $("pub-file-path").value = item.path;
      await inspectAndShowPath(item.path, $("pub-file-meta"));
    });

    $("btn-robot-upload").addEventListener("click", () => $("robot-file-input").click());
    setupDropzone("robot-dropzone", "robot-file-input", async (file) => {
      try {
        const rec = await uploadRawFile(file, $("robot-file-meta"));
        STATE.pubUpload = rec;
        $("robot-file-label").textContent = rec.filename;
        $("robot-file-meta").textContent = `${formatBytes(rec.sizeBytes)} · SHA-256 ${rec.sha256.slice(0, 12)}`;
      } catch (err) {
        $("robot-file-meta").textContent = err.message;
        toast(err.message, "err");
      }
    });

    $("btn-pub-dryrun").addEventListener("click", () => submitRobotPublish(true));
    $("robot-publish-form").addEventListener("submit", (e) => {
      e.preventDefault();
      submitRobotPublish(false);
    });

    // Push Job & Rollout Forms
    $("push-job-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      try {
        const job = await apiFetch("/api/jobs/start", {
          json: {
            deviceId: $("push-device-id").value.trim(),
            releaseId: $("push-release-id").value,
          },
        });
        toast(`已下发至 ${job.deviceId}`, "ok");
        closeDrawer("drawer-push");
        setView("devices");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("create-rollout-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      try {
        const ro = await apiFetch("/api/rollouts", {
          json: {
            releaseId: $("ro-release-id").value,
            rolloutId: $("ro-id").value.trim() || undefined,
            preset: $("ro-preset").value,
            canaryMaxDevices: Number($("ro-canary-max").value || 1),
            maxFailureCount: Number($("ro-max-fail").value || 1),
          },
        });
        toast(`灰度 ${ro.rolloutId} 已创建`, "ok");
        closeDrawer("drawer-rollout");
        setView("rollouts");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    // Android APK Form
    $("btn-apk-upload").addEventListener("click", () => $("apk-file-input").click());
    setupDropzone("apk-dropzone", "apk-file-input", async (file) => {
      try {
        const rec = await uploadRawFile(file, $("apk-file-meta"));
        STATE.apkSourceMode = "upload";
        STATE.apkUpload = rec;
        STATE.apkLocalPath = "";
        $("apk-file-label").textContent = rec.filename;
        $("apk-file-meta").textContent = `${formatBytes(rec.sizeBytes)} · SHA-256 ${rec.sha256.slice(0, 12)}`;
      } catch (err) {
        $("apk-file-meta").textContent = err.message;
        toast(err.message, "err");
      }
    });

    $("btn-apk-pick").addEventListener("click", async () => {
      try {
        const res = await apiFetch("/api/pick-path", { json: { kind: "apk" } });
        if (!res.supported) {
          toast("当前环境不支持本机弹窗，请直接拖入 APK", "err");
          return;
        }
        if (res.path) {
          const info = await apiFetch("/api/inspect-path", { json: { path: res.path } });
          if (!info.exists) return;
          STATE.apkSourceMode = "local_file";
          STATE.apkLocalPath = info.path;
          STATE.apkUpload = null;
          $("apk-file-label").textContent = info.name;
          $("apk-file-meta").textContent = `${formatBytes(info.sizeBytes)} · SHA-256 ${info.sha256.slice(0, 12)}`;
        }
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("local-apk-chips").addEventListener("click", async (e) => {
      const btn = e.target.closest("[data-local-apk-idx]");
      if (!btn) return;
      const item = STATE.localArtifacts.apks[Number(btn.dataset.localApkIdx)];
      if (!item) return;
      try {
        $("apk-file-meta").textContent = "正在校验本机 APK…";
        const info = await apiFetch("/api/inspect-path", { json: { path: item.path } });
        STATE.apkSourceMode = "local_file";
        STATE.apkLocalPath = info.path;
        STATE.apkUpload = null;
        $("apk-channel").value = item.channelHint || "qa";
        $("apk-file-label").textContent = info.name;
        $("apk-file-meta").textContent = `${formatBytes(info.sizeBytes)} · SHA-256 ${info.sha256.slice(0, 12)}`;
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("apk-channel").addEventListener("change", () => {
      const ch = $("apk-channel").value;
      const cur = STATE.overview?.android?.[ch];
      if (cur) $("apk-version-code").value = String(Number(cur.versionCode) + 1);
    });

    $("btn-apk-dryrun").addEventListener("click", () => submitAndroidPublish(true));
    $("android-publish-form").addEventListener("submit", (e) => {
      e.preventDefault();
      submitAndroidPublish(false);
    });

    // Settings Form
    $("settings-form").addEventListener("submit", async (e) => {
      e.preventDefault();
      try {
        const updated = await apiFetch("/api/config", {
          json: {
            cloudUrl: $("cfg-cloud-url").value.trim(),
            apiToken: $("cfg-api-token").value.trim() || undefined,
            ossBucket: $("cfg-oss-bucket").value.trim(),
            ossEndpoint: $("cfg-oss-endpoint").value.trim(),
            ossAccessKeyId: $("cfg-oss-ak-id").value.trim() || undefined,
            ossAccessKeySecret: $("cfg-oss-ak-secret").value.trim() || undefined,
            defaultModel: $("cfg-model").value.trim(),
            defaultHardwareRevision: $("cfg-hw").value.trim(),
            signingKeyId: $("cfg-key-id").value.trim(),
            opsJumpHost: $("cfg-ops-host").value.trim(),
            opsJumpUser: $("cfg-ops-user").value.trim(),
            opsJumpKey: $("cfg-ops-key").value.trim(),
            opsRobotUser: $("cfg-ops-robot-user").value.trim(),
            opsRobotPassword: $("cfg-ops-robot-pwd").value.trim() || undefined,
            defaultOwnerEmail: $("cfg-owner-email").value.trim(),
            ecsInstanceId: $("cfg-ecs-instance").value.trim(),
            ecsAccessKeyId: $("cfg-ecs-ak-id").value.trim() || undefined,
            ecsAccessKeySecret: $("cfg-ecs-ak-secret").value.trim() || undefined,
          },
        });
        $("cfg-api-token").value = "";
        $("cfg-oss-ak-id").value = "";
        $("cfg-oss-ak-secret").value = "";
        $("cfg-ops-robot-pwd").value = "";
        $("cfg-ecs-ak-id").value = "";
        $("cfg-ecs-ak-secret").value = "";
        populateSettingsForm(updated);
        toast("配置已保存", "ok");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
      }
    });

    $("btn-test-conn").addEventListener("click", async () => {
      const box = $("conn-test-result");
      box.innerHTML = "正在检测连接…";
      box.classList.remove("hidden");
      try {
        const h = await apiFetch("/api/health");
        const rows = [
          `<div class="step-row">${
            h.cloudReachable ? '<span class="badge badge-ok">正常</span>' : '<span class="badge badge-err">失败</span>'
          }<strong>云端可达</strong><span class="sub-line">${esc(h.cloudVersion || h.error || "")}</span></div>`,
          `<div class="step-row">${
            h.authOk ? '<span class="badge badge-ok">通过</span>' : '<span class="badge badge-warn">未通过</span>'
          }<strong>接口鉴权</strong><span class="sub-line">${esc(h.authOk ? "令牌有效" : h.error || "")}</span></div>`,
          `<div class="step-row">${
            h.releasesApiReady
              ? '<span class="badge badge-ok">已支持</span>'
              : '<span class="badge badge-warn">待升级</span>'
          }<strong>云端版本登记</strong><span class="sub-line">${
            h.releasesApiReady ? "支持一键登记" : "云端服务尚未升级"
          }</span></div>`,
          `<div class="step-row">${
            h.ossConfigured ? '<span class="badge badge-ok">已配置</span>' : '<span class="badge badge-warn">未配置</span>'
          }<strong>对象存储凭证</strong></div>`,
        ];
        box.innerHTML = rows.join("");
      } catch (err) {
        box.innerHTML = `<div style="color:var(--err-text)">${esc(err.message)}</div>`;
      }
    });
  }

  bindEvents();
  refreshAll(false);
  loadLocalArtifacts();
})();
