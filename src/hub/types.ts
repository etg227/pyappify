export interface UpdatePreview {
  installed_commit: string | null;
  target_commit: string;
  available: boolean;
  commits: string[];
  commit_count: number;
  python_version: string;
  requires_tk: boolean;
}

export interface ApplicationView {
  tool: {
    id: string;
    title: string;
    description: string;
    repository: string;
    branch: string;
    platforms: string[];
    channel: string;
  };
  supported: boolean;
  installed_commit: string | null;
  previous_commit: string | null;
  python_executable: string | null;
  running: boolean;
  busy: boolean;
  update: UpdatePreview | null;
}
