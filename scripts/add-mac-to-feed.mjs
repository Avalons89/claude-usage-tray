// Prints the update feed with the universal Mac app added for both Mac architectures.
// Usage: node add-mac-to-feed.mjs <feed.json, may be missing> <version> <url> <signature>
import { existsSync, readFileSync } from 'node:fs';

const [file, version, url, signature] = process.argv.slice(2);
const feed = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : { version, platforms: {} };
feed.pub_date ??= new Date().toISOString();
for (const platform of ['darwin-aarch64', 'darwin-x86_64']) {
  feed.platforms[platform] = { signature, url };
}
console.log(JSON.stringify(feed, null, 2));
