import assert from 'node:assert/strict';
import test from 'node:test';

import { convertNativeSource } from './generate-fivem.mjs';

test('normalizes EmmyLua types into typeless documentation', () => {
 const result = convertNativeSource('---@param value string string description\n---@return boolean Success.\nfunction normalize(value) end');
 assert.match(result.declarations, /---@param value string description/);
 assert.match(result.declarations, /---@return Success\./);
 assert.doesNotMatch(result.declarations, /---@param value string string/);
});

test('converts the documented AddAudioSubmixOutput example', () => {
	const result = convertNativeSource([
		'---Adds an output for the specified audio submix.',
		'---@param submixId integer Source submix.',
		'---@param outputSubmixId integer Destination submix.',
		'function AddAudioSubmixOutput(submixId, outputSubmixId) end',
	].join('\n'));

	assert.match(result.declarations, /function AddAudioSubmixOutput\(submixId: number, outputSubmixId: number\): \(\) end/);
	assert.match(result.declarations, /---Adds an output for the specified audio submix\./);
});

test('converts several return annotations to a value pack', () => {
	const result = convertNativeSource([
		'---@return boolean, integer',
		'function getValue() end',
	].join('\n'));

	assert.match(result.declarations, /function getValue\(\): \(boolean, number\) end/);
});

test('converts optional callbacks into optional callable types', () => {
	const result = convertNativeSource([
		'---@param callback? function Optional callback.',
		'function register(callback) end',
	].join('\n'));

	assert.match(result.declarations, /callback: \(\(\.\.\.any\) -> any\)\?/);
});

test('keeps declared aliases and uses them in signatures', () => {
	const result = convertNativeSource([
		'---@alias PedId integer',
		'---@param ped PedId',
		'function getPed(ped) end',
	].join('\n'));

	assert.match(result.declarations, /typedef PedId = number/);
	assert.match(result.declarations, /ped: PedId/);
});

test('retains native overloads as separate declarations', () => {
	const result = convertNativeSource([
		'---@param value string',
		'---@return integer',
		'function parse(value) end',
		'---@param value integer',
		'---@return string',
		'function parse(value) end',
	].join('\n'));

	assert.equal((result.declarations.match(/function parse\(/g) ?? []).length, 2);
});

test('preserves links, examples, and deprecated markers', () => {
	const result = convertNativeSource([
		'---[Native Documentation](https://docs.fivem.net/natives/?_0x1234)',
		'---```lua',
		'---example()',
		'---```',
		'---@deprecated',
		'function legacy() end',
	].join('\n'));

	assert.match(result.declarations, /https:\/\/docs\.fivem\.net\/natives/);
	assert.match(result.declarations, /---example\(\)/);
	assert.match(result.declarations, /---@deprecated/);
});

test('converts generic classes, optional fields, and dynamic table indices', () => {
	const result = convertNativeSource([
		'---@class Box<T>',
		'---@field value T',
		'---@field label? string',
		'---@field [string] any',
	].join('\n'));

	assert.match(result.declarations, /typedef Box<T> = \{ value: T, label\?: string, \[string\]: any \}/);
});

test('converts documented alias unions and overload annotations', () => {
	const result = convertNativeSource([
		'---@alias Mode',
		"---| 'one'",
		"---| 'two'",
		'---@overload fun(value: integer): string',
		'---@param value Mode',
		'---@return integer',
		'function parse(value) end',
	].join('\n'));

	assert.match(result.declarations, /typedef Mode = 'one' \| 'two'/);
	assert.match(result.declarations, /function parse\(value: number\): string end/);
	assert.match(result.declarations, /function parse\(value: Mode\): number end/);
});

test('fails on annotations whose types cannot be represented', () => {
	assert.throws(
		() => convertNativeSource('---@param value vector99\nfunction use(value) end', { sourcePath: 'bad.lua' }),
		/unsupported type 'vector99'.*bad\.lua/,
	);
});

test('explains the raw NUI native and Lua helper without changing their parameters', () => {
 const raw = convertNativeSource('---@param jsonString string\nfunction SendNuiMessage(jsonString) end');
 const helper = convertNativeSource('---@param data table\nfunction SendNUIMessage(data) end');
 assert.match(raw.declarations, /Raw native/);
 assert.match(raw.declarations, /jsonString: string/);
 assert.match(helper.declarations, /Lua helper/);
 assert.match(helper.declarations, /data: table/);
});
