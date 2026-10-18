// A shape implemented in TypeScript, imported by a relative path.
//
// `codets` erases the types while it parses, so this file runs without a build step
// — which is the property the verification script checks by running it.
export interface Release {
  name: string;
  version: string;
}

export function summarise(releases: Release[]): string {
  const names: string[] = releases.map((release: Release): string => release.name);
  return `${names.join(' + ')}: ${releases.length} releases described`;
}

export function describe(release: Release): string {
  return `${release.name}@${release.version}`;
}
