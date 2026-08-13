# Moves post bodies off the markdown `posts.body` column and into Action Text,
# because the editor is now Lexxy and Lexxy edits Action Text rich text.
#
# The conversion runs kramdown one last time — the same renderer the app used at
# runtime until this commit — so nothing written before the switch is lost or
# has to be retyped. That is also why the kramdown gems are still in the
# Gemfile: this migration is their only remaining caller.
#
# Reversible, but ONLY structurally. `down` restores the column and writes back
# `to_plain_text`, which is prose with its formatting flattened. Rolling back
# does not resurrect the original markdown, and it says so rather than
# pretending to be lossless.
class ConvertPostBodiesToRichText < ActiveRecord::Migration[8.1]
  # A local model, not Post. The real Post declares `has_rich_text :body`, which
  # would shadow the very column this migration has to read.
  class MigratingPost < ActiveRecord::Base
    self.table_name = "posts"
  end

  def up
    MigratingPost.reset_column_information

    say_with_time "converting #{MigratingPost.count} post body/bodies to rich text" do
      MigratingPost.find_each do |row|
        markdown = row.read_attribute(:body).to_s
        next if markdown.blank?

        html = Kramdown::Document.new(
          markdown,
          input: "GFM",
          hard_wrap: false,
          auto_ids: true,
          # No rouge here: Lexxy highlights code in the browser, so baking
          # rouge's <span> soup into the stored HTML would leave dead markup
          # that the new stylesheet does not target.
          syntax_highlighter: nil
        ).to_html

        ActionText::RichText.create!(
          record_type: "Post",
          record_id: row.id,
          name: "body",
          body: html
        )
      end
    end

    remove_column :posts, :body
  end

  def down
    add_column :posts, :body, :text, null: false, default: ""
    MigratingPost.reset_column_information

    say_with_time "restoring plain-text bodies (FORMATTING IS NOT RECOVERED)" do
      ActionText::RichText.where(record_type: "Post", name: "body").find_each do |rich_text|
        MigratingPost.where(id: rich_text.record_id)
                     .update_all(body: rich_text.to_plain_text)
      end
    end

    ActionText::RichText.where(record_type: "Post", name: "body").delete_all
  end
end
