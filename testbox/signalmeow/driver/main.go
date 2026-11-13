// The testbox's signalmeow client: mautrix-signal's pkg/signalmeow (Go, with libsignal's FFI for
// the cryptography, and none of presage or libsignal-service) driven from a command line, the
// way xtask drives presage-cli with `podman exec`:
//
//	signalmeow-driver --db /signalmeow/a.db link <device name>      (prints "URL <sgnl://linkdevice…>", then "linked <aci>")
//	signalmeow-driver --db … whoami                                 (the ACI)
//	signalmeow-driver --db … send <aci> <text>                      (stdin when <text> is "-")
//	signalmeow-driver --db … send-file <aci> <path> <text>          (uploaded to the CDN the server names)
//	signalmeow-driver --db … send-group <master key, hex or base64> <text>
//	signalmeow-driver --db … read <aci> <timestamp>…                (read receipts)
//	signalmeow-driver --db … sync [seconds]                         (receive until the queue is empty)
//	signalmeow-driver --db … list-messages <aci or group id>        (what this device sent and received)
//	signalmeow-driver --db … serve                                  (JSON lines on stdin and stdout: below)
//
// Every command but `link` and `list-messages` connects the websockets first and prints what
// arrives meanwhile, as presage-cli does: "From <aci> @ <timestamp>: <text>".
//
// `serve` stays connected: it reads one JSON command a line ({"id":1,"op":"send","to":"<aci>",
// "text":"…"}; ops send, send-group, read, whoami, quit) and writes one JSON line for each answer
// ({"id":1,"ok":true,"timestamp":…}) and each event ({"event":"message",…}).
//
// SIGNALMEOW_BOX_CONFIG (the box's desktop-config.json) points signalmeow at the box
// (testbox/patches/signalmeow-local.patch).
package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/google/uuid"
	_ "github.com/mattn/go-sqlite3"
	"github.com/rs/zerolog"
	"go.mau.fi/util/dbutil"

	"go.mau.fi/mautrix-signal/pkg/libsignalgo"
	"go.mau.fi/mautrix-signal/pkg/signalmeow"
	"go.mau.fi/mautrix-signal/pkg/signalmeow/events"
	"go.mau.fi/mautrix-signal/pkg/signalmeow/protobuf/signalpb"
	"go.mau.fi/mautrix-signal/pkg/signalmeow/store"
	"go.mau.fi/mautrix-signal/pkg/signalmeow/types"
	"google.golang.org/protobuf/proto"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func usage() error {
	return errors.New("usage: signalmeow-driver --db <path> link|whoami|send|send-group|read|sync|list-messages|serve …")
}

func run(args []string) error {
	var dbPath string
	level := zerolog.InfoLevel
	for len(args) >= 2 && strings.HasPrefix(args[0], "--") {
		switch args[0] {
		case "--db":
			dbPath = args[1]
		case "--log-level":
			var err error
			if level, err = zerolog.ParseLevel(args[1]); err != nil {
				return err
			}
		default:
			return usage()
		}
		args = args[2:]
	}
	if dbPath == "" || len(args) == 0 {
		return usage()
	}
	command, args := args[0], args[1:]

	// Logs go to stderr; stdout is the protocol's.
	log := zerolog.New(zerolog.ConsoleWriter{Out: os.Stderr, TimeFormat: time.RFC3339Nano, NoColor: true}).
		Level(level).With().Timestamp().Logger()
	signalmeow.SetLogger(log)

	ctx, stop := signal.NotifyContext(log.WithContext(context.Background()), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if command == "list-messages" {
		if len(args) != 1 {
			return usage()
		}
		return listMessages(dbPath+".messages", args[0])
	}

	container, err := openStore(ctx, dbPath, log)
	if err != nil {
		return err
	}
	if command == "link" {
		return link(ctx, container, args, log)
	}
	devices, err := container.GetAllDevices(ctx)
	if err != nil {
		return err
	}
	if len(devices) != 1 {
		return fmt.Errorf("%d devices in %s: link first", len(devices), dbPath)
	}
	if command == "whoami" {
		fmt.Println(devices[0].ACI)
		return nil
	}

	s := newSession(ctx, devices[0], dbPath+".messages", command == "serve", log)
	if err := s.connect(30 * time.Second); err != nil {
		return err
	}
	defer s.close()

	switch command {
	case "send":
		if len(args) != 2 {
			return usage()
		}
		text := args[1]
		if text == "-" {
			all, err := io.ReadAll(os.Stdin)
			if err != nil {
				return err
			}
			text = string(all)
		}
		ts, err := s.sendText(args[0], text)
		if err != nil {
			return err
		}
		fmt.Printf("To %s @ %d: %s\n", args[0], ts, oneLine(text))
		return s.settle()
	case "send-file":
		if len(args) != 3 {
			return usage()
		}
		ts, err := s.sendFile(args[0], args[1], args[2])
		if err != nil {
			return err
		}
		fmt.Printf("To %s @ %d: %s [file %s]\n", args[0], ts, oneLine(args[2]), args[1])
		return s.settle()
	case "send-group":
		if len(args) != 2 {
			return usage()
		}
		ts, err := s.sendGroup(args[0], args[1])
		if err != nil {
			return err
		}
		fmt.Printf("To group %s @ %d: %s\n", args[0], ts, oneLine(args[1]))
		return s.settle()
	case "read":
		if len(args) < 2 {
			return usage()
		}
		return s.sendRead(args[0], args[1:])
	case "sync":
		wait := 20 * time.Second
		if len(args) == 1 {
			seconds, err := strconv.Atoi(args[0])
			if err != nil {
				return err
			}
			wait = time.Duration(seconds) * time.Second
		}
		select {
		case <-s.queueEmpty:
		case <-time.After(wait):
			return fmt.Errorf("the queue was not empty after %s", wait)
		case <-ctx.Done():
		}
		return s.settle()
	case "serve":
		return s.serve(ctx, os.Stdin)
	}
	return usage()
}

func oneLine(text string) string {
	return strings.NewReplacer("\n", `\n`, "\r", `\r`).Replace(text)
}

func openStore(ctx context.Context, path string, log zerolog.Logger) (*store.Container, error) {
	// The signalmeow store's own database, as the bridge's: WAL and foreign keys.
	db, err := dbutil.NewWithDialect(
		"file:"+path+"?_txlock=immediate&_busy_timeout=10000&_journal_mode=WAL&_foreign_keys=on", "sqlite3")
	if err != nil {
		return nil, err
	}
	container := store.NewStore(db, dbutil.ZeroLogger(log.With().Str("db", "signalmeow").Logger()))
	if err := container.Upgrade(ctx); err != nil {
		return nil, fmt.Errorf("upgrading the store: %w", err)
	}
	return container, nil
}

// link provisions as a secondary device. Prints the linking URL, which a primary adds
// (presage-cli `link-device --url`), then waits for it to.
func link(ctx context.Context, container *store.Container, args []string, log zerolog.Logger) error {
	name := "signalmeow"
	if len(args) > 0 {
		name = args[0]
	}
	var linked *store.DeviceData
	for response := range signalmeow.PerformProvisioning(ctx, container, name, false) {
		switch response.State {
		case signalmeow.StateProvisioningURLReceived:
			fmt.Println("URL", response.ProvisioningURL)
		case signalmeow.StateProvisioningDataReceived:
			linked = response.ProvisioningData
		case signalmeow.StateProvisioningError:
			return fmt.Errorf("provisioning: %w", response.Err)
		}
	}
	if linked == nil {
		return errors.New("provisioning ended without linking")
	}
	fmt.Println("linked", linked.ACI)
	return nil
}

// A message's line in the messages file: what `list-messages` reads.
type logged struct {
	Direction string `json:"direction"` // "from" or "to"
	Peer      string `json:"peer"`      // an ACI, or a group's identifier
	Group     bool   `json:"group,omitempty"`
	Timestamp uint64 `json:"timestamp"`
	Text      string `json:"text"`
	Files     int    `json:"files,omitempty"`
}

func listMessages(path, peer string) error {
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	} else if err != nil {
		return err
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(nil, 16<<20)
	for scanner.Scan() {
		var entry logged
		if err := json.Unmarshal(scanner.Bytes(), &entry); err != nil {
			return err
		}
		if entry.Peer != peer {
			continue
		}
		direction := "From"
		if entry.Direction == "to" {
			direction = "To"
		}
		fmt.Printf("%s %s @ %d: %s\n", direction, entry.Peer, entry.Timestamp, oneLine(entry.Text))
	}
	return scanner.Err()
}

type session struct {
	ctx    context.Context
	cancel context.CancelFunc
	log    zerolog.Logger
	device *store.Device
	client *signalmeow.Client
	json   bool

	outMu sync.Mutex // stdout, and the messages file
	file  *os.File

	connected  chan struct{}
	queueEmpty chan struct{}
	queueOnce  sync.Once
	lastTS     atomic.Uint64
	received   atomic.Uint64
	loggedOut  atomic.Bool
}

func newSession(ctx context.Context, device *store.Device, messagesPath string, asJSON bool, log zerolog.Logger) *session {
	ctx, cancel := context.WithCancel(ctx)
	s := &session{
		ctx: ctx, cancel: cancel, log: log, device: device, json: asJSON,
		connected: make(chan struct{}), queueEmpty: make(chan struct{}),
	}
	var err error
	s.file, err = os.OpenFile(messagesPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		panic(err)
	}
	s.client = signalmeow.NewClient(device, log.With().Str("component", "signalmeow").Logger(), s.handle)
	return s
}

func (s *session) close() {
	if err := s.client.StopReceiveLoops(); err != nil {
		s.log.Err(err).Msg("stopping the receive loops")
	}
	s.cancel()
	_ = s.file.Close()
}

func (s *session) connect(timeout time.Duration) error {
	statuses, err := s.client.StartReceiveLoops(s.ctx)
	if err != nil {
		return fmt.Errorf("starting the receive loops: %w", err)
	}
	var once sync.Once
	go func() {
		for status := range statuses {
			s.log.Info().Stringer("event", status.Event).Err(status.Err).Msg("connection")
			switch status.Event {
			case signalmeow.SignalConnectionEventConnected:
				once.Do(func() { close(s.connected) })
			case signalmeow.SignalConnectionEventLoggedOut:
				s.loggedOut.Store(true)
				s.emit("logged-out", map[string]any{"error": fmt.Sprint(status.Err)}, "logged out: %v", status.Err)
			}
		}
	}()
	select {
	case <-s.connected:
		return nil
	case <-time.After(timeout):
		return fmt.Errorf("not connected after %s", timeout)
	case <-s.ctx.Done():
		return s.ctx.Err()
	}
}

// settle waits a moment for the receiving that goes on while a command runs, as presage-cli's
// send does, then returns: what it sent is acknowledged by then, and what arrived is printed.
func (s *session) settle() error {
	select {
	case <-s.queueEmpty:
	case <-time.After(5 * time.Second):
	case <-s.ctx.Done():
	}
	time.Sleep(500 * time.Millisecond)
	return nil
}

func (s *session) nextTimestamp() uint64 {
	for {
		last := s.lastTS.Load()
		now := uint64(time.Now().UnixMilli())
		if now <= last {
			now = last + 1
		}
		if s.lastTS.CompareAndSwap(last, now) {
			return now
		}
	}
}

// emit writes an event: a JSON line in `serve`, `format` otherwise.
func (s *session) emit(event string, fields map[string]any, format string, args ...any) {
	s.outMu.Lock()
	defer s.outMu.Unlock()
	if s.json {
		fields["event"] = event
		line, _ := json.Marshal(fields)
		fmt.Printf("%s\n", line)
		return
	}
	fmt.Printf(format+"\n", args...)
}

func (s *session) record(entry logged) {
	line, _ := json.Marshal(entry)
	s.outMu.Lock()
	defer s.outMu.Unlock()
	_, _ = s.file.Write(append(line, '\n'))
}

func (s *session) handle(event events.SignalEvent) bool {
	switch event := event.(type) {
	case *events.ChatEvent:
		return s.handleChat(event)
	case *events.Receipt:
		var timestamps []string
		for _, ts := range event.Content.GetTimestamp() {
			timestamps = append(timestamps, strconv.FormatUint(ts, 10))
		}
		kind := strings.ToLower(event.Content.GetType().String())
		s.emit("receipt", map[string]any{"type": kind, "from": event.Sender.String(), "timestamps": event.Content.GetTimestamp()},
			"Receipt %s from %s for %s", kind, event.Sender, strings.Join(timestamps, ","))
	case *events.DecryptionError:
		// The wording presage-cli's loss check looks for (xtask checks/presage.rs LOST).
		s.emit("decryption-error", map[string]any{"from": event.Sender.String(), "timestamp": event.Timestamp, "error": fmt.Sprint(event.Err)},
			"failed to decrypt a message from <ACI:%s> of %d: %v", event.Sender, event.Timestamp, event.Err)
	case *events.QueueEmpty:
		s.queueOnce.Do(func() { close(s.queueEmpty) })
		s.emit("queue-empty", map[string]any{}, "queue empty")
	case *events.LoggedOut:
		s.loggedOut.Store(true)
		s.emit("logged-out", map[string]any{"error": fmt.Sprint(event.Error)}, "logged out: %v", event.Error)
	}
	return true
}

func (s *session) handleChat(event *events.ChatEvent) bool {
	own := event.Info.Sender == s.device.ACI
	switch content := event.Event.(type) {
	case *signalpb.DataMessage:
		s.received.Add(1)
		group := content.GetGroupV2() != nil
		direction, who := "from", event.Info.Sender.String()
		label := "From"
		if own {
			// What this account's other device sent, as a transcript.
			direction, label = "to", "To"
			who = event.Info.ChatID
		}
		peer := event.Info.ChatID
		if !group && !own {
			peer = who
		}
		text := content.GetBody()
		ts := content.GetTimestamp()
		s.record(logged{Direction: direction, Peer: peer, Group: group, Timestamp: ts, Text: text, Files: len(content.GetAttachments())})
		where := ""
		if group {
			where = " [group " + event.Info.ChatID + "]"
		}
		files := ""
		if n := len(content.GetAttachments()); n > 0 {
			files = fmt.Sprintf(" [%d attachment(s)]", n)
		}
		if r := content.GetReaction(); r != nil {
			files += fmt.Sprintf(" [reaction %s to %d]", r.GetEmoji(), r.GetTargetSentTimestamp())
		}
		fields := map[string]any{
			"from": event.Info.Sender.String(), "chat": event.Info.ChatID, "group": group, "own": own,
			"timestamp": ts, "text": text, "attachments": len(content.GetAttachments()),
		}
		if r := content.GetReaction(); r != nil {
			fields["reaction"] = r.GetEmoji()
			fields["reaction_target"] = r.GetTargetSentTimestamp()
		}
		s.emit("message", fields, "%s %s @ %d: %s%s%s", label, who, ts, oneLine(text), where, files)
		// Downloaded and decrypted here, which checks the CDN and the digest and MAC: what
		// arrived is its size and hash.
		for _, pointer := range content.GetAttachments() {
			data, err := signalmeow.DownloadAttachmentWithPointer(s.ctx, pointer, nil, nil)
			if err != nil {
				s.emit("attachment-error", map[string]any{"timestamp": ts, "error": err.Error()},
					"failed to download an attachment of %d: %v", ts, err)
				continue
			}
			sum := sha256.Sum256(data)
			s.emit("attachment", map[string]any{
				"timestamp": ts, "name": pointer.GetFileName(), "type": pointer.GetContentType(),
				"size": len(data), "sha256": hex.EncodeToString(sum[:]),
			}, "Attachment of %d: %s %s %d bytes sha256:%x", ts, pointer.GetFileName(), pointer.GetContentType(), len(data), sum)
		}
	case *signalpb.EditMessage:
		dm := content.GetDataMessage()
		s.emit("edit", map[string]any{"from": event.Info.Sender.String(), "target": content.GetTargetSentTimestamp(), "text": dm.GetBody()},
			"Edit from %s of %d: %s", event.Info.Sender, content.GetTargetSentTimestamp(), oneLine(dm.GetBody()))
	case *signalpb.TypingMessage:
		s.emit("typing", map[string]any{"from": event.Info.Sender.String(), "action": content.GetAction().String()},
			"Typing %s from %s", content.GetAction(), event.Info.Sender)
	}
	return true
}

// sendReaction reacts with `emoji` to the message `author` sent at `target`, in the chat with `to`.
func (s *session) sendReaction(to, author string, target uint64, emoji string) (uint64, error) {
	aci, err := uuid.Parse(to)
	if err != nil {
		return 0, fmt.Errorf("%q: %w", to, err)
	}
	authorACI, err := uuid.Parse(author)
	if err != nil {
		return 0, fmt.Errorf("%q: %w", author, err)
	}
	ts := s.nextTimestamp()
	content := signalmeow.WrapDataMessage(&signalpb.DataMessage{
		Timestamp:               &ts,
		RequiredProtocolVersion: proto.Uint32(uint32(signalpb.DataMessage_REACTIONS)),
		Reaction: &signalpb.DataMessage_Reaction{
			Emoji:                 proto.String(emoji),
			Remove:                proto.Bool(false),
			TargetAuthorAciBinary: authorACI[:],
			TargetSentTimestamp:   proto.Uint64(target),
		},
	})
	result := s.client.SendMessage(s.ctx, libsignalgo.NewACIServiceID(aci), content)
	if !result.WasSuccessful {
		if result.Error != nil {
			return 0, fmt.Errorf("reacting to %s: %w", to, result.Error)
		}
		return 0, fmt.Errorf("reacting to %s failed", to)
	}
	return ts, nil
}

// sendEdit changes this account's message sent at `target`, in the chat with `to`, to `text`.
func (s *session) sendEdit(to string, target uint64, text string) (uint64, error) {
	aci, err := uuid.Parse(to)
	if err != nil {
		return 0, fmt.Errorf("%q: %w", to, err)
	}
	ts := s.nextTimestamp()
	content := signalmeow.WrapEditMessage(&signalpb.EditMessage{
		TargetSentTimestamp: proto.Uint64(target),
		DataMessage:         &signalpb.DataMessage{Body: &text, Timestamp: &ts},
	})
	result := s.client.SendMessage(s.ctx, libsignalgo.NewACIServiceID(aci), content)
	if !result.WasSuccessful {
		if result.Error != nil {
			return 0, fmt.Errorf("editing for %s: %w", to, result.Error)
		}
		return 0, fmt.Errorf("editing for %s failed", to)
	}
	return ts, nil
}

func (s *session) sendText(to, text string) (uint64, error) {
	aci, err := uuid.Parse(to)
	if err != nil {
		return 0, fmt.Errorf("%q: %w", to, err)
	}
	ts := s.nextTimestamp()
	content := signalmeow.WrapDataMessage(&signalpb.DataMessage{Body: &text, Timestamp: &ts})
	result := s.client.SendMessage(s.ctx, libsignalgo.NewACIServiceID(aci), content)
	if !result.WasSuccessful {
		if result.Error != nil {
			return 0, fmt.Errorf("sending to %s: %w", to, result.Error)
		}
		return 0, fmt.Errorf("sending to %s failed", to)
	}
	s.record(logged{Direction: "to", Peer: to, Timestamp: ts, Text: text})
	return ts, nil
}

// sendFile sends `text` with the file at `path` to `to`, uploaded as signalmeow does: encrypted,
// padded, to the CDN the server names (CDN3's tus, or CDN0's POST policy).
func (s *session) sendFile(to, path, text string) (uint64, error) {
	aci, err := uuid.Parse(to)
	if err != nil {
		return 0, fmt.Errorf("%q: %w", to, err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	pointer, err := s.client.UploadAttachment(s.ctx, data)
	if err != nil {
		return 0, fmt.Errorf("uploading %s: %w", path, err)
	}
	name := filepath.Base(path)
	contentType := mime.TypeByExtension(filepath.Ext(path))
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	pointer.FileName, pointer.ContentType = &name, &contentType
	ts := s.nextTimestamp()
	dm := &signalpb.DataMessage{Body: &text, Timestamp: &ts, Attachments: []*signalpb.AttachmentPointer{pointer}}
	result := s.client.SendMessage(s.ctx, libsignalgo.NewACIServiceID(aci), signalmeow.WrapDataMessage(dm))
	if !result.WasSuccessful {
		return 0, fmt.Errorf("sending %s to %s: %v", path, to, result.Error)
	}
	sum := sha256.Sum256(data)
	s.record(logged{Direction: "to", Peer: to, Timestamp: ts, Text: text, Files: 1})
	fmt.Fprintf(os.Stderr, "sent %s: %d bytes sha256:%x\n", name, len(data), sum)
	return ts, nil
}

func (s *session) sendGroup(masterKey, text string) (uint64, error) {
	// Base64, as signalmeow serializes it; or hex, as presage-cli and b33p name a group's key.
	if raw, err := hex.DecodeString(masterKey); err == nil && len(raw) == 32 {
		masterKey = base64.StdEncoding.EncodeToString(raw)
	}
	key := types.SerializedGroupMasterKey(masterKey)
	if raw, err := base64.StdEncoding.DecodeString(masterKey); err != nil || len(raw) != 32 {
		return 0, fmt.Errorf("master key %q is neither 32 bytes of base64 nor of hex", masterKey)
	}
	gid, err := s.client.StoreMasterKey(s.ctx, key)
	if err != nil {
		return 0, err
	}
	ts := s.nextTimestamp()
	content := signalmeow.WrapDataMessage(&signalpb.DataMessage{Body: &text, Timestamp: &ts})
	result, err := s.client.SendGroupMessage(s.ctx, gid, content)
	if err != nil {
		return 0, fmt.Errorf("sending to group %s: %w", gid, err)
	}
	if len(result.FailedToSendTo) > 0 {
		return 0, fmt.Errorf("sending to group %s: %d of %d members failed: %w", gid,
			len(result.FailedToSendTo), len(result.FailedToSendTo)+len(result.SuccessfullySentTo), result.FailureError())
	}
	s.record(logged{Direction: "to", Peer: gid.String(), Group: true, Timestamp: ts, Text: text})
	return ts, nil
}

func (s *session) sendRead(to string, timestamps []string) error {
	aci, err := uuid.Parse(to)
	if err != nil {
		return err
	}
	var parsed []uint64
	for _, raw := range timestamps {
		ts, err := strconv.ParseUint(raw, 10, 64)
		if err != nil {
			return err
		}
		parsed = append(parsed, ts)
	}
	result := s.client.SendMessage(s.ctx, libsignalgo.NewACIServiceID(aci), signalmeow.ReadReceptMessageForTimestamps(parsed))
	if !result.WasSuccessful {
		return fmt.Errorf("sending the read receipt to %s: %v", to, result.Error)
	}
	return nil
}

type command struct {
	ID         json.RawMessage `json:"id"`
	Op         string          `json:"op"`
	To         string          `json:"to"`
	MasterKey  string          `json:"master_key"`
	Path       string          `json:"path"`
	Text       string          `json:"text"`
	Timestamps []uint64        `json:"timestamps"`

	// A reaction's or an edit's target: the author's ACI, the time the message was sent at, the emoji.
	Author string `json:"author"`
	Target uint64 `json:"target"`
	Emoji  string `json:"emoji"`
}

func (s *session) serve(ctx context.Context, in io.Reader) error {
	s.emit("ready", map[string]any{"aci": s.device.ACI.String()}, "ready")
	reader := bufio.NewReaderSize(in, 1<<20)
	var wait sync.WaitGroup
	defer wait.Wait()
	for {
		line, err := reader.ReadBytes('\n')
		if len(strings.TrimSpace(string(line))) > 0 {
			var cmd command
			if jsonErr := json.Unmarshal(line, &cmd); jsonErr != nil {
				s.emit("answer", map[string]any{"ok": false, "error": jsonErr.Error()}, "")
			} else if cmd.Op == "quit" {
				return nil
			} else {
				// Concurrently: a slow send doesn't hold up the next, as clients do.
				wait.Add(1)
				go func() {
					defer wait.Done()
					s.answer(cmd)
				}()
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
		if ctx.Err() != nil {
			return nil
		}
	}
}

func (s *session) answer(cmd command) {
	fields := map[string]any{"ok": true}
	if len(cmd.ID) > 0 {
		fields["id"] = cmd.ID
	}
	var err error
	switch cmd.Op {
	case "whoami":
		fields["aci"] = s.device.ACI.String()
	case "send":
		var ts uint64
		ts, err = s.sendText(cmd.To, cmd.Text)
		fields["timestamp"] = ts
	case "send-file":
		var ts uint64
		ts, err = s.sendFile(cmd.To, cmd.Path, cmd.Text)
		fields["timestamp"] = ts
	case "send-group":
		var ts uint64
		ts, err = s.sendGroup(cmd.MasterKey, cmd.Text)
		fields["timestamp"] = ts
	case "react":
		var ts uint64
		ts, err = s.sendReaction(cmd.To, cmd.Author, cmd.Target, cmd.Emoji)
		fields["timestamp"] = ts
	case "edit":
		var ts uint64
		ts, err = s.sendEdit(cmd.To, cmd.Target, cmd.Text)
		fields["timestamp"] = ts
	case "read":
		timestamps := make([]string, len(cmd.Timestamps))
		for i, ts := range cmd.Timestamps {
			timestamps[i] = strconv.FormatUint(ts, 10)
		}
		err = s.sendRead(cmd.To, timestamps)
	default:
		err = fmt.Errorf("unknown op %q", cmd.Op)
	}
	if err != nil {
		fields["ok"] = false
		fields["error"] = err.Error()
	}
	s.emit("answer", fields, "")
}
