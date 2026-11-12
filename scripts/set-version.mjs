// Every commit on main gets a higher version, so installed copies see it as an update.
// Needs full git history; run from the repo root.
import { execSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';

const file = 'src-tauri/tauri.conf.json';
const commits = execSync('git rev-list --count HEAD').toString().trim();
const config = JSON.parse(readFileSync(file, 'utf8'));
config.version = `0.1.${commits}`;
writeFileSync(file, JSON.stringify(config, null, 2));
console.log(config.version);
