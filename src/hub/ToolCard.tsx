import type {ApplicationView} from './types';

interface Props {
  application: ApplicationView;
  busy: boolean;
  error?: string;
  onCheck: () => void;
  onAction: (action: 'start' | 'show' | 'stop' | 'restore') => void;
  onLog: () => void;
}

export default function ToolCard({application: app, busy, error, onCheck, onAction, onLog}: Props) {
  const installed = Boolean(app.installed_commit);
  const state = app.state_error ? '需要处理' : busy ? '正在处理' : app.running ? '正在运行' : installed ? '已安装' : '未安装';
  const disabled = busy || !app.supported || Boolean(app.state_error);
  return (
    <article className="tool-card" aria-busy={busy}>
      <div className="tool-card-heading">
        <span className="tool-mark" aria-hidden="true">{app.tool.title.charAt(0)}</span>
        <div><h2>{app.tool.title}</h2><span className="tool-platform">{app.tool.platforms.join(' · ')} · 开发预览</span></div>
        <span className={`status ${app.running ? 'running' : ''}`} role="status">{state}</span>
      </div>
      <p className="tool-description">{app.tool.description}</p>
      <div className="tool-version">{installed ? `当前提交 ${app.installed_commit?.slice(0, 7)}` : '安装后即可独立启动'}
        {app.update?.available && installed && <span className="update-dot">有更新</span>}
      </div>
      {!app.supported && <p className="tool-note">这个工具暂不支持当前系统。</p>}
      {(error || app.state_error) && <div className="tool-error" role="alert"><strong>操作未完成</strong><details><summary>查看原因</summary>{error || app.state_error}</details></div>}
      <div className="tool-actions">
        {installed && <button className="primary" disabled={disabled} onClick={() => onAction(app.running ? 'show' : 'start')}>
          {app.running ? '打开窗口' : '启动'}
        </button>}
        <button className={installed ? '' : 'primary'} disabled={disabled} onClick={onCheck}>
          {installed ? '检查更新' : '安装'}
        </button>
        {app.running && <button disabled={busy || Boolean(app.state_error)} onClick={() => onAction('stop')}>停止</button>}
        {(installed || app.state_error) && <button disabled={busy} onClick={onLog}>日志</button>}
        {app.previous_commit && <button disabled={disabled || app.running} onClick={() => onAction('restore')}>恢复上一版</button>}
      </div>
    </article>
  );
}
