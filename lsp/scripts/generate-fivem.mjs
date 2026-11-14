import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const SOURCE_COMMIT = "ef5f08e5e1d4a176586a5befbe0a9ff8400c58fc";
const SOURCE_REPOSITORY = "overextended/fivem-lls-addon";
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const RUNTIME_SOURCES = ["citizen.lua", "env.lua", "event.lua", "json.lua", "msgpack.lua", "promise.lua"].map(
	(file) => `library/runtime/${file}`,
);
const RUNTIME_CONTEXTS = new Map([
	["GetPlayerIdentifiers", "server"],
	["GetPlayerTokens", "server"],
	["PerformHttpRequest", "server"],
	["PerformHttpRequestAwait", "server"],
	["GetPlayers", "server"],
	["TriggerClientEvent", "server"],
	["TriggerLatentClientEvent", "server"],
	["RegisterNUICallback", "client"],
	["SendNUIMessage", "client"],
	["TriggerServerEvent", "client"],
	["TriggerLatentServerEvent", "client"],
]);
const RUNTIME_PARAMETER_OVERRIDES = new Map([["library/runtime/msgpack.lua:msgpack.extend_get:ext_id", "number"]]);
const RUNTIME_GLOBALS = [
	["source", "unknown", "shared"],
	["GlobalState", "StateBag", "shared"],
	["LocalPlayer", "EntityInterface", "client"],
	["exports", "(...any) -> any", "shared"],
];
const PRIMITIVES = new Set([
	"any",
	"boolean",
	"buffer",
	"false",
	"integer",
	"never",
	"nil",
	"number",
	"string",
	"table",
	"thread",
	"true",
	"unknown",
	"userdata",
	"vector2",
	"vector3",
	"vector4",
]);

function splitTopLevel(source, separators) {
	const parts = [];
	const closing = [];
	let quote = null;
	let escaped = false;
	let start = 0;
	for (let index = 0; index < source.length; index++) {
		const character = source[index];
		if (quote) {
			if (escaped) escaped = false;
			else if (character === "\\") escaped = true;
			else if (character === quote) quote = null;
			continue;
		}
		if (character === '"' || character === "'") {
			quote = character;
			continue;
		}
		if (character === "(") closing.push(")");
		else if (character === "{") closing.push("}");
		else if (character === "[") closing.push("]");
		else if (character === "<") closing.push(">");
		else if (character === ">" && source[index - 1] !== "-" && closing.at(-1) === ">") closing.pop();
		else if (closing.at(-1) === character) closing.pop();
		else if (closing.length === 0 && separators.has(character)) {
			parts.push(source.slice(start, index).trim());
			start = index + 1;
		}
	}
	parts.push(source.slice(start).trim());
	return parts.filter(Boolean);
}

function readTypePrefix(input) {
	let index = input.search(/\S/);
	if (index < 0) throw new Error("missing type annotation");
	const start = index;
	const closing = [];
	let quote = null;
	let escaped = false;
	let previous = "";
	while (index < input.length) {
		const character = input[index];
		if (quote) {
			if (escaped) escaped = false;
			else if (character === "\\") escaped = true;
			else if (character === quote) quote = null;
			index++;
			continue;
		}
		if (character === '"' || character === "'") {
			quote = character;
			previous = character;
			index++;
			continue;
		}
		if (/\s/.test(character)) {
			let next = index;
			while (/\s/.test(input[next] ?? "")) next++;
			if (closing.length || ["|", "&", "<", ",", ":"].includes(previous) || ["|", "&"].includes(input[next])) {
				index = next;
				continue;
			}
			break;
		}
		if (character === "," && closing.length === 0) break;
		if (character === "(") closing.push(")");
		else if (character === "{") closing.push("}");
		else if (character === "[") closing.push("]");
		else if (character === "<") closing.push(">");
		else if (character === ">" && input[index - 1] !== "-" && closing.at(-1) === ">") closing.pop();
		else if (closing.at(-1) === character) closing.pop();
		previous = character;
		index++;
	}
	return { type: input.slice(start, index).trim(), rest: input.slice(index).trimStart() };
}

function findClosing(source, open, left = "(", right = ")") {
	let depth = 0;
	let quote = null;
	let escaped = false;
	for (let index = open; index < source.length; index++) {
		const character = source[index];
		if (quote) {
			if (escaped) escaped = false;
			else if (character === "\\") escaped = true;
			else if (character === quote) quote = null;
			continue;
		}
		if (character === '"' || character === "'") quote = character;
		else if (character === left) depth++;
		else if (character === right && --depth === 0) return index;
	}
	return -1;
}

function parseReturnTypes(source) {
	let rest = source.trim().replace(/^:/, "").trim();
	if (!rest) return ["any"];
	const returns = [];
	while (rest) {
		const annotation = readTypePrefix(rest);
		returns.push(annotation.type);
		rest = annotation.rest;
		if (rest.startsWith(",")) {
			rest = rest.slice(1).trimStart();
			continue;
		}
		const parts = splitTopLevel(rest, new Set([","]));
		if (parts.length > 1 && /^[\w?]+$/.test(parts[0])) {
			rest = rest.slice(rest.indexOf(",") + 1).trimStart();
			continue;
		}
		break;
	}
	return returns;
}

function parseFunctionType(type) {
	const open = type.indexOf("(");
	const close = findClosing(type, open);
	const arrow = type.indexOf("->", close);
	if (open < 0 || close < 0 || arrow < 0) throw new Error(`unsupported callable type '${type}'`);
	return { parameters: type.slice(open + 1, close), returns: type.slice(arrow + 2).trim() };
}

function convertEmmyFunctionType(type, aliases, sourcePath, report) {
	const open = type.indexOf("(");
	const close = findClosing(type, open);
	if (!type.startsWith("fun") || open < 0 || close < 0) {
		throw new Error(`unsupported function type '${type}' in ${sourcePath}`);
	}
	const parameters = parseParameterList(type.slice(open + 1, close)).map((parameter, index) => {
		if (parameter === "...") return "...any";
		const separator = splitTopLevel(parameter, new Set([":"]));
		const named = separator.length > 1;
		let name = named ? separator[0].trim() : `parameter${index + 1}`;
		const optional = name.endsWith("?");
		name = name.replace(/\?$/, "");
		const parameterType = named ? parameter.slice(parameter.indexOf(":") + 1).trim() : parameter;
		let converted = convertType(parameterType, aliases, sourcePath, report);
		if (optional) converted = converted.includes("->") ? `(${converted})?` : `${converted}?`;
		return name === "..." ? `...${converted}` : `${name}: ${converted}`;
	});
	const returns = parseReturnTypes(type.slice(close + 1)).map((value) => convertType(value, aliases, sourcePath, report));
	return `(${parameters.join(", ")}) -> ${returns.length === 1 ? returns[0] : `(${returns.join(", ")})`}`;
}

function convertRecordType(type, aliases, sourcePath, report) {
	const fields = splitTopLevel(type.slice(1, -1), new Set([",", ";"])).map((field) => {
		const parts = splitTopLevel(field, new Set([":"]));
		if (parts.length < 2) throw new Error(`unsupported record field '${field}' in ${sourcePath}`);
		const name = parts[0].trim();
		return `${name}: ${convertType(field.slice(field.indexOf(":") + 1), aliases, sourcePath, report)}`;
	});
	return `{ ${fields.join(", ")} }`;
}

function convertType(type, aliases, sourcePath, report) {
	const value = type.trim();
	if (!value) throw new Error(`missing type annotation in ${sourcePath}`);
	if (value === "...") {
		if (report) report.mappedVariadicReturn++;
		return "any";
	}
	if (value.endsWith("?")) {
		const base = convertType(value.slice(0, -1), aliases, sourcePath, report);
		return base.includes("->") ? `(${base})?` : `${base}?`;
	}
	const unions = splitTopLevel(value, new Set(["|"]));
	if (unions.length > 1) return unions.map((part) => convertType(part, aliases, sourcePath, report)).join(" | ");
	const intersections = splitTopLevel(value, new Set(["&"]));
	if (intersections.length > 1) return intersections.map((part) => convertType(part, aliases, sourcePath, report)).join(" & ");
	if (value.startsWith("fun(")) return convertEmmyFunctionType(value, aliases, sourcePath, report);
	if (value.startsWith("(") && value.endsWith(")")) {
		const inner = value.slice(1, -1).trim();
		if (splitTopLevel(inner, new Set([","])).length > 1) {
			return `(${splitTopLevel(inner, new Set([","]))
				.map((part) => convertType(part, aliases, sourcePath, report))
				.join(", ")})`;
		}
		return `(${convertType(inner, aliases, sourcePath, report)})`;
	}
	if (value === "function" || value === "fun") {
		if (report) report.mappedFunction++;
		return "(...any) -> any";
	}
	if (value === "lightuserdata") {
		if (report) report.mappedLightuserdata++;
		return "userdata";
	}
	if (value === "vector") {
		if (report) report.mappedVector++;
		return "vector2 | vector3 | vector4";
	}
	if (/^(['"]).*\1$/.test(value)) return value;
	if (value === "table") {
		if (report) report.mappedTable++;
		return "table";
	}
	if (value.endsWith("[]")) return `{ ${convertType(value.slice(0, -2), aliases, sourcePath, report)} }`;
	if (value.startsWith("{") && value.endsWith("}")) return convertRecordType(value, aliases, sourcePath, report);
	if (PRIMITIVES.has(value) || aliases.has(value)) return value;
	const genericStart = value.indexOf("<");
	if (genericStart > 0 && value.endsWith(">")) {
		const name = value.slice(0, genericStart).trim();
		const args = splitTopLevel(value.slice(genericStart + 1, -1), new Set([","]));
		if (name === "table") {
			if (report) report.mappedTable++;
			if (args.length === 2)
				return `{ [${convertType(args[0], aliases, sourcePath, report)}]: ${convertType(args[1], aliases, sourcePath, report)} }`;
			if (args.length === 1) return `{ ${convertType(args[0], aliases, sourcePath, report)} }`;
		}
		if (!aliases.has(name)) throw new Error(`unsupported type '${name}' in ${sourcePath}`);
		return `${name}<${args.map((part) => convertType(part, aliases, sourcePath, report)).join(", ")}>`;
	}
	throw new Error(`unsupported type '${value}' in ${sourcePath}`);
}

function scanTypeNames(sources) {
	const names = new Set();
	for (const { source } of sources) {
		for (const line of source.split(/\r?\n/)) {
			const match = line.match(/^---@(?:class|alias)\s+([\w]+)/);
			if (match) names.add(match[1]);
		}
	}
	return names;
}

function parseTypeDefinitions(sources, knownTypes, report) {
	const definitions = new Map();
	for (const { source, sourcePath } of sources) {
		let active = null;
		for (const line of source.split(/\r?\n/)) {
			let match = line.match(/^---@class\s+([\w]+)(?:<([^>]+)>)?(?:\s*:\s*(.+))?\s*$/);
			if (match) {
				active = {
					name: match[1],
					generics: match[2]?.split(",").map((item) => item.trim()) ?? [],
					base: match[3]?.trim(),
					fields: [],
					sourcePath,
				};
				definitions.set(active.name, active);
				continue;
			}
			match = line.match(/^---@alias\s+([\w]+)(?:<([^>]+)>)?(?:\s+(.+))?$/);
			if (match) {
				active = {
					name: match[1],
					generics: match[2]?.split(",").map((item) => item.trim()) ?? [],
					type: match[3]?.trim(),
					variants: [],
					fields: [],
					sourcePath,
					alias: true,
				};
				definitions.set(active.name, active);
				continue;
			}
			match = line.match(/^---\|\s*(.+)$/);
			if (match && active?.alias) {
				active.variants.push(readTypePrefix(match[1]).type);
				continue;
			}
			match = line.match(/^---@field\s+(\S+)\s+(.+)$/);
			if (match && active) {
				const optional = match[1].endsWith("?");
				let type = match[2].trim().startsWith("fun(") ? match[2].trim() : readTypePrefix(match[2]).type;
				if (type.startsWith("fun(self,")) {
					const owner = `${active.name}${active.generics.length ? `<${active.generics.join(", ")}>` : ""}`;
					type = type.replace("fun(self,", `fun(self: ${owner},`);
				}
				active.fields.push({ name: match[1].replace(/\?$/, ""), optional, type });
				continue;
			}
			if (!line.startsWith("---") && line.trim()) active = null;
		}
	}
	const aliases = new Map();
	for (const [name, definition] of definitions) {
		const visible = new Set([...knownTypes, ...definitions.keys(), ...definition.generics]);
		let type;
		if (definition.variants?.length)
			type = definition.variants.map((variant) => convertType(variant, visible, definition.sourcePath, report)).join(" | ");
		else if (definition.type) type = convertType(definition.type, visible, definition.sourcePath, report);
		else {
			const fields = definition.fields.map((field) => {
				const name = field.name.startsWith("[") ? field.name : field.optional ? `${field.name}?` : field.name;
				return `${name}: ${convertType(field.type, visible, definition.sourcePath, report)}`;
			});
			type = `{ ${fields.join(", ")} }`;
			if (definition.base) type = `${convertType(definition.base, visible, definition.sourcePath, report)} & ${type}`;
		}
		const generic = definition.generics.length ? `<${definition.generics.join(", ")}>` : "";
		aliases.set(name, `${generic} = ${type}`);
	}
	return aliases;
}

function parseAnnotations(docLines) {
	const params = new Map();
	const returns = [];
	const overloads = [];
	let vararg;
	for (const line of docLines) {
		let match = line.match(/^---@param\s+(\S+)\s+(.+)$/);
		if (match) {
			const rawName = match[1];
			const annotation = readTypePrefix(match[2].split(";", 1)[0]);
			params.set(rawName.replace(/\?$/, ""), { type: annotation.type, optional: rawName.endsWith("?") });
			continue;
		}
		match = line.match(/^---@vararg\s+(.+)$/);
		if (match) {
			vararg = readTypePrefix(match[1].split(";", 1)[0]).type;
			continue;
		}
		match = line.match(/^---@return\s+(.+)$/);
		if (match) {
			returns.push(...parseReturnTypes(match[1].split(";", 1)[0]));
			continue;
		}
		match = line.match(/^---@overload\s+(.+)$/);
		if (match) {
			overloads.push(match[1].trim());
		}
	}
	return { params, returns, overloads, vararg };
}

function parseParameterList(source) {
	return splitTopLevel(source, new Set([","]))
		.map((parameter) => parameter.trim())
		.filter(Boolean);
}

function documentationLines(lines) {
 return lines.flatMap((line) => {
  const param = line.match(/^---@param\s+(\S+)\s+(.+)$/);
  if (param) {
   const description = readTypePrefix(param[2]).rest.replace(/^[;#]\s*/, '').trim();
   return description ? ['---@param ' + param[1].replace(/\?$/, '') + ' ' + description] : [];
  }
  const returned = line.match(/^---@return\s+(.+)$/);
  if (returned) {
   let rest = returned[1];
   for (;;) { const annotation = readTypePrefix(rest); rest = annotation.rest.trim(); if (!rest.startsWith(',')) break; rest = rest.slice(1).trimStart(); }
   rest = rest.replace(/^[;#]\s*/, '').trim();
   return rest ? ['---@return ' + rest] : [];
  }
  return /^---@(?:vararg|overload|generic|alias|class|field|type)\b/.test(line) ? [] : [line];
 });
}

function parseNativeHeader(source, sourcePath, report, knownTypes = new Set()) {
	const records = [];
	const definitions = new Map();
	const typeAliases = parseTypeDefinitions([{ source, sourcePath }], knownTypes, report);
	let docs = [];
	for (const line of source.split(/\r?\n/)) {
		if (line.startsWith("---")) {
			if (/^---@(?:class|alias|field)\b/.test(line) || /^---\|/.test(line)) {
				docs = [];
				continue;
			}
			docs.push(line);
			continue;
		}
		const functionMatch = line.match(/^function\s+([\w.]+)\s*\((.*)\)\s*(?:end)?\s*$/);
		if (functionMatch) {
			const { params, returns, overloads, vararg } = parseAnnotations(docs);
			const aliasNames = new Set([...knownTypes, ...typeAliases.keys()]);
			const args = parseParameterList(functionMatch[2]);
			const typedArgs = args.map((argument) => {
				if (argument === "...") {
					if (report) report.openVarargs++;
					return `...: ${convertType(vararg ?? "any", aliasNames, sourcePath, report)}`;
				}
				const parameter =
					params.get(argument) ??
					(RUNTIME_PARAMETER_OVERRIDES.has(`${sourcePath}:${functionMatch[1]}:${argument}`)
						? { type: RUNTIME_PARAMETER_OVERRIDES.get(`${sourcePath}:${functionMatch[1]}:${argument}`), optional: false }
						: undefined);
				if (!parameter) throw new Error(`missing @param annotation for '${argument}' in ${sourcePath}: ${functionMatch[1]}`);
				const type = convertType(parameter.type, aliasNames, sourcePath, report);
				return `${argument}: ${parameter.optional ? (type.includes("->") ? `(${type})?` : `${type}?`) : type}`;
			});
			const returnTypes = returns.map((type) => convertType(type, aliasNames, sourcePath, report));
			const returnType = returnTypes.length === 0 ? "()" : returnTypes.length === 1 ? returnTypes[0] : `(${returnTypes.join(", ")})`;
			if (functionMatch[1] === "SendNuiMessage") docs.push("---Raw native: accepts a JSON string. Use SendNUIMessage for a Lua table.");
			if (functionMatch[1] === "SendNUIMessage") docs.push("---Lua helper: encodes a Lua table with json.encode before calling SendNuiMessage.");
			const declaration =
				`${documentationLines(docs).join("\n")}\nfunction ${functionMatch[1]}(${typedArgs.join(", ")}): ${returnType} end`.trim();
			const context = docs.map((doc) => doc.match(/^---\*\*`[^`]+` `(client|server|shared)`\*\*/)?.[1]).find(Boolean) ?? "shared";
			records.push({ declaration, context, name: functionMatch[1] });
			for (const overload of overloads) {
				const signature = convertEmmyFunctionType(overload, aliasNames, sourcePath, report);
				const parsed = parseFunctionType(signature);
				const overloadArgs = parseParameterList(parsed.parameters).map((parameter) =>
					parameter.startsWith("...") ? `...: ${parameter.slice(3).trim()}` : parameter,
				);
				records.push({
					declaration: `function ${functionMatch[1]}(${overloadArgs.join(", ")}): ${parsed.returns} end`,
					context,
					name: functionMatch[1],
				});
			}
			definitions.set(functionMatch[1], { args: typedArgs, returns: returnType, context });
			docs = [];
			continue;
		}
		const aliasMatch = line.match(/^([\w.]+)\s*=\s*([\w.]+)\s*$/);
		if (aliasMatch && definitions.has(aliasMatch[2])) {
			const target = definitions.get(aliasMatch[2]);
			const description = docs.filter((doc) => doc.includes("@deprecated")).join("\n");
			records.push({
				declaration: `${description}\nfunction ${aliasMatch[1]}(${target.args.join(", ")}): ${target.returns} end`.trim(),
				context: target.context,
				name: aliasMatch[1],
			});
			definitions.set(aliasMatch[1], target);
			docs = [];
			continue;
		}
		if (line.trim() === "" && docs.length) {
			docs.push("");
			continue;
		}
		const typedField = line.match(/^([\w.]+)\s*=/);
		const typeAnnotation = docs.find((doc) => doc.startsWith("---@type "));
		if (typedField && typeAnnotation) {
			const name = typedField[1];
			records.push({
				name,
				type: convertType(
					readTypePrefix(typeAnnotation.slice("---@type ".length)).type,
					new Set([...knownTypes, ...typeAliases.keys()]),
					sourcePath,
					report,
				),
				context: "shared",
			});
			docs = [];
			continue;
		}
		docs = [];
	}
	return { records, aliases: typeAliases };
}

export function convertNativeSource(source, { sourcePath = "<memory>" } = {}) {
	const report = {
		mappedTable: 0,
		mappedFunction: 0,
		mappedLightuserdata: 0,
		mappedVector: 0,
		mappedVariadicReturn: 0,
		openVarargs: 0,
	};
	const typeNames = scanTypeNames([{ source }]);
	const parsed = parseNativeHeader(source, sourcePath, report, typeNames);
	const contexts = [...new Set(parsed.records.map((record) => record.context))];
	const declarations = [
		...parsed.aliases.entries().map(([name, type]) => `typedef ${name}${type}`),
		...parsed.records.map((record) => record.declaration).filter(Boolean),
	].join("\n\n");
	return { declarations: `${declarations}\n`, contexts, records: parsed.records, aliases: parsed.aliases, report };
}

async function fetchSource(destination) {
	const response = await fetch(`https://codeload.github.com/${SOURCE_REPOSITORY}/tar.gz/${SOURCE_COMMIT}`, {
		headers: { "User-Agent": "LuaT-FiveM-definition-generator" },
	});
	if (!response.ok) throw new Error(`could not download pinned FiveM definitions: HTTP ${response.status}`);
	execFileSync("tar", ["-xzf", "-", "-C", destination, "--strip-components=1"], {
		input: Buffer.from(await response.arrayBuffer()),
	});
}

function listNativeFiles(sourceRoot) {
	return ["GTAV", "CFX-NATIVE"]
		.flatMap((directory) => {
			const absolute = path.join(sourceRoot, "library", "natives", directory);
			return readdirSync(absolute, { withFileTypes: true })
				.filter((entry) => entry.isFile() && entry.name.endsWith(".lua"))
				.map((entry) => path.join(absolute, entry.name));
		})
		.sort();
}

function functionFieldType(declaration) {
	const start = declaration.lastIndexOf("\nfunction ") + 1;
	const match = declaration.slice(start).match(/^function\s+[\w.]+\((.*)\):\s*(.*)\s+end$/s);
	if (!match) throw new Error(`could not read generated function signature: ${declaration}`);
	const parameters = parseParameterList(match[1]).map((parameter) => parameter.replace(/^\.\.\.\s*:\s*/, "..."));
	return `(${parameters.join(", ")}) -> ${match[2]}`;
}

function runtimeNamespaceTypes(records, report) {
	const namespaces = new Map();
	for (const record of records) {
		const separator = record.name.indexOf(".");
		if (separator < 0) continue;
		const root = record.name.slice(0, separator);
		const field = record.name.slice(separator + 1);
		if (field.includes(".")) continue;
		const fields = namespaces.get(root) ?? new Map();
		fields.set(field, [...(fields.get(field) ?? []), record.type ?? functionFieldType(record.declaration)]);
		namespaces.set(root, fields);
	}
	return [...namespaces].map(([name, fields]) => {
		if (name === "json") fields.set("null", ["userdata"]);
		if (name === "json") fields.set("sentinel", ["userdata"]);
		if (name === "msgpack") fields.set("sentinel", ["userdata"]);
		const members = [...fields]
			.sort(([left], [right]) => left.localeCompare(right))
			.map(([field, signatures]) => `${field}: ${signatures.join(" & ")}`);
		return [
			`typedef ${name[0].toUpperCase()}${name.slice(1)}Api = { ${members.join(", ")} }`,
			name,
			`${name[0].toUpperCase()}${name.slice(1)}Api`,
		];
	});
}

async function generate({ sourceRoot, outputRoot }) {
	let temporaryRoot;
	if (!sourceRoot) {
		temporaryRoot = mkdtempSync(path.join(os.tmpdir(), "luat-fivem-"));
		await fetchSource(temporaryRoot);
		sourceRoot = temporaryRoot;
	}
	const outputByContext = new Map([
		["client", []],
		["server", []],
		["shared", []],
	]);
	const runtimeByContext = new Map([
		["client", []],
		["server", []],
		["shared", []],
	]);
	const report = {
		sourceCommit: SOURCE_COMMIT,
		nativeFiles: [],
		runtimeFiles: [],
		functions: 0,
		runtimeFunctions: 0,
		mappedTable: 0,
		mappedFunction: 0,
		mappedLightuserdata: 0,
		mappedVector: 0,
		mappedVariadicReturn: 0,
		openVarargs: 0,
		parameterOverrides: [],
	};
	try {
		const nativeFiles = listNativeFiles(sourceRoot);
		const runtimeFiles = RUNTIME_SOURCES.map((relative) => path.join(sourceRoot, relative));
		const allFiles = [...nativeFiles, ...runtimeFiles];
		const sources = allFiles.map((file) => ({
			file,
			relative: path.relative(sourceRoot, file).replaceAll(path.sep, "/"),
			source: readFileSync(file, "utf8"),
		}));
		const knownTypes = scanTypeNames(sources);
		const runtimeSources = sources.filter((entry) => RUNTIME_SOURCES.includes(entry.relative));
		const runtimeTypes = parseTypeDefinitions(
			runtimeSources.map((entry) => ({ source: entry.source, sourcePath: entry.relative })),
			knownTypes,
			report,
		);
		const allRuntimeRecords = [];
		for (const { relative, source } of sources.filter((entry) => nativeFiles.includes(entry.file))) {
			const converted = parseNativeHeader(source, relative, report, knownTypes);
			const contexts = [...new Set(converted.records.map((record) => record.context))];
			for (const context of contexts) {
				for (const [name, type] of converted.aliases) outputByContext.get(context).push(`typedef ${name}${type}`);
			}
			for (const record of converted.records) {
				outputByContext.get(record.context).push(record.declaration);
				report.functions++;
			}
			report.nativeFiles.push(relative);
		}
		for (const { relative, source } of runtimeSources) {
			const converted = parseNativeHeader(source, relative, report, knownTypes);
			for (const record of converted.records) {
				record.context = RUNTIME_CONTEXTS.get(record.name) ?? "shared";
				if (record.declaration) {
					runtimeByContext.get(record.context).push(record.declaration);
					report.runtimeFunctions++;
				}
				allRuntimeRecords.push(record);
			}
			report.runtimeFiles.push(relative);
		}
		for (const [key, type] of RUNTIME_PARAMETER_OVERRIDES) {
			report.parameterOverrides.push({
				source: key,
				mappedType: type,
				reason: "upstream leaves ext_id unannotated; settype documents the same value as an optional number",
			});
		}
		const namespaceTypes = runtimeNamespaceTypes(allRuntimeRecords, report);
		for (const [name, type] of runtimeTypes) runtimeByContext.get("shared").push(`typedef ${name}${type}`);
		for (const [type, name, apiType] of namespaceTypes) {
			runtimeByContext.get("shared").push(type);
			runtimeByContext.get("shared").push(`declare ${name}: ${apiType}`);
		}
		for (const [name, type, context] of RUNTIME_GLOBALS) {
			const target = context === "client" ? "client" : context === "server" ? "server" : "shared";
			runtimeByContext.get(target).push(`declare ${name}: ${type}`);
		}
		mkdirSync(outputRoot, { recursive: true });
		for (const [context, declarations] of outputByContext) {
			const header = `-- Generated by lsp/scripts/generate-fivem.mjs from ${SOURCE_REPOSITORY}@${SOURCE_COMMIT}.\n`;
			writeFileSync(path.join(outputRoot, `fivem-${context}.d.luat`), header + declarations.join("\n\n") + "\n");
			writeFileSync(path.join(outputRoot, `runtime-${context}.d.luat`), header + runtimeByContext.get(context).join("\n\n") + "\n");
		}
		const manifest = [
			"# Definition sources",
            ...readFileSync(path.join(ROOT, "lsp/definitions/LUA-SOURCE.md"), "utf8").split("\n"),
            "## FiveM definitions",
			"",
			`- Repository: https://github.com/${SOURCE_REPOSITORY}`,
			`- Commit: ${SOURCE_COMMIT}`,
			"- Included paths: GTA V and shared CFX native declarations under `library/natives/GTAV` and `library/natives/CFX-NATIVE`; runtime types from the six listed `library/runtime/` sources.",
			"- Excluded paths: RedM natives, `library/runtime/manifest.lua`, and `library/runtime/luaglm.lua` (LuaT owns vector types).",
			"- Runtime context mapping: client-only `RegisterNUICallback`, `SendNUIMessage`, `TriggerServerEvent`, `TriggerLatentServerEvent`, and `LocalPlayer`; server-only `GetPlayerIdentifiers`, `GetPlayerTokens`, `PerformHttpRequest`, `PerformHttpRequestAwait`, `GetPlayers`, `TriggerClientEvent`, and `TriggerLatentClientEvent`; all other included runtime APIs are shared. This is based on the pinned source documentation and FiveM runtime behavior.",
			"- Explicit adaptation: `msgpack.extend_get` has no `@param ext_id` annotation upstream; it is typed as `number` based on `msgpack.settype` in the same pinned source and recorded in `conversion-report.json`.",
			"- License: MIT; see [LICENSE](LICENSE).",
			"- Lua NUI helper behavior: [FiveM scheduler](https://github.com/citizenfx/fivem/blob/master/data/shared/citizen/scripting/lua/scheduler.lua) defines `SendNUIMessage(message)` by calling `SendNuiMessage(json.encode(message))`. Both names are exposed; the raw native keeps its JSON string parameter.",
			"",
			...report.nativeFiles.map((file) => `- \`${file}\``),
			...report.runtimeFiles.map((file) => `- \`${file}\``),
			"",
		].join("\n");
		writeFileSync(path.join(outputRoot, "SOURCE.md"), manifest);
		report.typeMappings = {
			table: "table",
			function: "(...any) -> any",
			lightuserdata: "userdata",
			vector: "vector2 | vector3 | vector4",
			variadicReturn: "any",
		};
		if (sourceRoot) writeFileSync(path.join(outputRoot, "LICENSE"), readFileSync(path.join(sourceRoot, "LICENSE")));
		writeFileSync(path.join(outputRoot, "conversion-report.json"), `${JSON.stringify(report, null, 2)}\n`);
		return report;
	} finally {
		if (temporaryRoot) rmSync(temporaryRoot, { recursive: true, force: true });
	}
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
	const sourceIndex = process.argv.indexOf("--source");
	const outputIndex = process.argv.indexOf("--output");
	const sourceRoot = sourceIndex >= 0 ? path.resolve(process.argv[sourceIndex + 1]) : undefined;
	const outputRoot = outputIndex >= 0 ? path.resolve(process.argv[outputIndex + 1]) : path.join(ROOT, "lsp", "definitions");
	generate({ sourceRoot, outputRoot })
		.then((report) => {
			process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
		})
		.catch((error) => {
			process.stderr.write(`${error.stack ?? error}\n`);
			process.exitCode = 1;
		});
}
