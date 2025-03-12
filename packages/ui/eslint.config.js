import { fileURLToPath } from 'node:url';

import { includeIgnoreFile } from '@eslint/compat';
import baseConfig from '@repo/eslint-config';
import ts from 'typescript-eslint';

import svelteConfig from './svelte.config.js';
const gitignorePath = fileURLToPath(new URL('./.gitignore', import.meta.url));

export default ts.config(
	includeIgnoreFile(gitignorePath),
	...baseConfig,

	{
		languageOptions: {
			parserOptions: {
				svelteConfig
			}
		}
	}
);
