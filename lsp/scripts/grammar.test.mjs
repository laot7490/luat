import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const grammar = JSON.parse(await readFile(new URL('../editors/vscode/syntaxes/luat.tmLanguage.json', import.meta.url), 'utf8'));

test('TextMate grammar scopes operators and protects comments and strings', () => {
	const patterns = grammar.patterns;
	const comments = patterns.findIndex(pattern => pattern.include === '#comments');
	const strings = patterns.findIndex(pattern => pattern.include === '#strings');
	const operators = patterns.findIndex(pattern => pattern.include === '#operators');
	assert.ok(comments >= 0 && comments < operators);
	assert.ok(strings >= 0 && strings < operators);

	const operatorPatterns = grammar.repository.operators.patterns;
	assert.equal(grammar.repository.keywords.patterns[0].match.includes('typedef'), true);
	const symbols = new RegExp(`^(?:${operatorPatterns[0].match})$`);
	const words = new RegExp(`^(?:${operatorPatterns[1].match})$`);
	for (const operator of ['~=', '==', '<=', '>=', '+=', '-=', '*=', '/=', '//=', '%=', '^=', '..=', '<<=', '>>=', '&=', '|=', '??', '?.', '::', '->', '<<', '>>', '..', '+', '-', '*', '/', '%', '^', '#', '=', '<', '>', '&', '|', '~']) {
		assert.equal(symbols.test(operator), true, operator);
		assert.equal(new RegExp(operatorPatterns[0].match).exec(operator)?.[0], operator);
	}
	for (const operator of ['and', 'or', 'not']) assert.equal(words.test(operator), true, operator);
	const keywords = grammar.repository.keywords.patterns[0].match;
	assert.equal(keywords.includes('export'), false);
	const slashComment = new RegExp(grammar.repository.comments.patterns.find(pattern => pattern.name === 'comment.line.double-slash.luat').match);
	assert.equal(slashComment.test('// comment'), true);
	assert.equal(slashComment.test('value // other'), false);
	const longComment = grammar.repository.comments.patterns.find(pattern => pattern.name === 'comment.block.long.luat');
	const longString = grammar.repository.strings.patterns.find(pattern => pattern.name === 'string.quoted.other.multiline.luat');
	assert.equal(new RegExp(longComment.begin).test('--[==['), true);
	assert.equal(longComment.end, '\\]\\1\\]');
	assert.equal(new RegExp(longString.begin).test('[==['), true);
	assert.equal(longString.end, longComment.end);
	const template = grammar.repository.strings.patterns.find(pattern => pattern.name === 'string.template.luat');
	const interpolation = template.patterns.find(pattern => pattern.name === 'meta.interpolation.luat');
	assert.ok(interpolation.patterns.some(pattern => pattern.include === '#operators'));
	const signatureOperators = grammar.repository['signature-parameters'].patterns;
	assert.ok(signatureOperators.some(pattern => pattern.include === '#operators'));
});
