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
  };

  const VIEW_META = {
    devices: { title: "设备车队", cta: "发布固件" },
    releases: { title: "机器狗版本", cta: "发布固件" },
    rollouts: { title: "灰度发布", cta: "新建灰度" },
    android: { title: "App 更新", cta: null },
    settings: { title: "云端配置", cta: null },
  };

  const ARTIFACT_LABELS = {
    brainstem_release: "核心固件",
    policy_package: "运动策略",
    component_release: "独立组件",
    release_bundle: "组合包",
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
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
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

  // Navigation
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
  }

  function openDrawer(id) {
    $(id).classList.remove("hidden");
  }

  function closeDrawer(id) {
    $(id).classList.add("hidden");
  }

  // Rendering Overview
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
    const devices = ov.devices || [];
    const releases = ov.releases || [];
    const rollouts = ov.rollouts || [];
    const activeRollouts = rollouts.filter((r) => r.status === "active").length;
    const prodApp = ov.android?.prod;

    $("stat-devices").textContent = String(devices.length);
    $("stat-releases").textContent = String(releases.length);
    $("stat-rollouts").textContent = String(activeRollouts);
    $("stat-android").textContent = prodApp ? `v${prodApp.versionName} (${prodApp.versionCode})` : "未发布";

    $("nav-count-devices").textContent = String(devices.length);
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

  function renderDevices(devices) {
    const q = ($("device-search").value || "").trim().toLowerCase();
    const filtered = (devices || []).filter((d) => {
      if (!q) return true;
      return (
        String(d.deviceId || "").toLowerCase().includes(q) ||
        String(d.firmwareVersion || "").toLowerCase().includes(q) ||
        String(d.releaseId || "").toLowerCase().includes(q)
      );
    });

    const datalist = $("known-devices-list");
    datalist.innerHTML = (devices || []).map((d) => `<option value="${esc(d.deviceId)}"></option>`).join("");

    const tbody = $("devices-tbody");
    if (!filtered.length) {
      tbody.innerHTML = `<tr><td colspan="6" class="empty-cell">暂无设备状态记录</td></tr>`;
      return;
    }

    tbody.innerHTML = filtered
      .map((d) => {
        const online = d.lastSeenAgeMs !== null && d.lastSeenAgeMs !== undefined && d.lastSeenAgeMs < 120000;
        const ready = d.brainstemReady && d.strategyReady;
        const statusBadge = online
          ? ready
            ? `<span class="badge badge-ok"><span class="dot dot-ok"></span>在线就绪</span>`
            : `<span class="badge badge-warn"><span class="dot dot-warn"></span>在线未就绪</span>`
          : `<span class="badge badge-neutral"><span class="dot dot-neutral"></span>离线</span>`;
        const profile = d.activeProfile || Object.keys(d.slots || {}).join(", ") || "—";
        return `
          <tr>
            <td class="mono">${esc(d.deviceId)}</td>
            <td>${statusBadge}</td>
            <td>
              <div>${esc(d.firmwareVersion || "—")}</div>
              <div class="sub-line mono">${esc(d.releaseId || "")}</div>
            </td>
            <td>${esc(profile)}</td>
            <td>${esc(formatAge(d.lastSeenAgeMs))}</td>
            <td class="col-actions">
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
        return `
          <tr>
            <td>
              <div class="mono">${esc(r.releaseId)}</div>
              <div class="sub-line mono">${esc((r.artifactSha256 || "").slice(0, 12))}</div>
            </td>
            <td>${esc(typeLabel + extra)}</td>
            <td><span class="badge badge-neutral">${esc(r.channel)}</span></td>
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

    // Suggest next Android versionCode
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
      renderDevices(ov.devices);
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

  // Push Job Drawer
  async function openPushDrawer({ deviceId = "", releaseId = "" } = {}) {
    $("push-device-id").value = deviceId;
    const select = $("push-release-id");
    let items = STATE.overview?.releases || [];
    if (deviceId) {
      try {
        const compat = await apiFetch(`/api/devices/${encodeURIComponent(deviceId)}/releases`);
        if (Array.isArray(compat) && compat.length) items = compat;
      } catch {
        // Fallback to global release catalog
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

  // Android Publish
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
    // Sidebar & topbar
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

    // Drawer close
    document.querySelectorAll("[data-close-drawer]").forEach((btn) => {
      btn.addEventListener("click", () => closeDrawer(btn.dataset.closeDrawer));
    });
    document.querySelectorAll(".drawer-backdrop").forEach((bg) => {
      bg.addEventListener("click", (e) => {
        if (e.target === bg) bg.classList.add("hidden");
      });
    });

    // Search & table delegations
    $("device-search").addEventListener("input", () => renderDevices(STATE.overview?.devices));
    $("release-search").addEventListener("input", () => renderReleases(STATE.overview?.releases));
    $("btn-manual-push").addEventListener("click", () => openPushDrawer());

    $("devices-tbody").addEventListener("click", (e) => {
      const btn = e.target.closest("[data-push-device]");
      if (btn) openPushDrawer({ deviceId: btn.dataset.pushDevice });
    });

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
      const act = btn.dataset.roAct;
      const rid = btn.dataset.roId;
      btn.disabled = true;
      try {
        await apiFetch(`/api/rollouts/${encodeURIComponent(rid)}/${act}`, { method: "POST", json: {} });
        toast("灰度状态已更新", "ok");
        await refreshAll(true);
      } catch (err) {
        toast(err.message, "err");
        btn.disabled = false;
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

    // Push Job Form
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

    // Create Rollout Form
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
      if (cur) {
        $("apk-version-code").value = String(Number(cur.versionCode) + 1);
      }
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
          },
        });
        $("cfg-api-token").value = "";
        $("cfg-oss-ak-id").value = "";
        $("cfg-oss-ak-secret").value = "";
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
